//! Compare tree changes as content: `cargo run --example content_diff`.
//! Uses a disposable repository; output is escaped byte data, not a unified patch.
use std::sync::atomic::AtomicBool;

use girt::content_diff::{BinaryMode, BlobContent, ContentDiff, DiffLimits, diff};
use girt::{
    EntryMode, InitKind, PackLimits, ReadLimits, Repository, Tree, TreeCompareLimits, TreeEntry,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let repo = Repository::init(root.path().join("repo"), InitKind::Bare)?;
    let store = repo.loose_objects()?;
    let old_blob = store.write_blob(b"heading\r\nold\n")?;
    let new_blob = store.write_blob(b"heading\r\nnew\xff")?;
    let old = store.write_tree(&Tree::new(vec![TreeEntry {
        name: b"example.txt".to_vec(),
        mode: EntryMode::Blob,
        id: old_blob,
    }])?)?;
    let new = store.write_tree(&Tree::new(vec![TreeEntry {
        name: b"example.txt".to_vec(),
        mode: EntryMode::Executable,
        id: new_blob,
    }])?)?;
    let objects = repo.objects(PackLimits::default())?;
    let cancel = AtomicBool::new(false);
    let limits = DiffLimits::default();
    let changes =
        objects.compare_trees(Some(old), Some(new), TreeCompareLimits::default(), &cancel)?;
    for change in changes {
        println!(
            "{}: {:?} -> {:?}",
            escaped(&change.path),
            change.old.map(|v| v.mode),
            change.new.map(|v| v.mode)
        );
        // Gitlinks require a submodule policy; the blob adapter deliberately rejects them.
        let content = BlobContent::read(
            &objects,
            &change,
            ReadLimits::default(),
            limits.max_input_bytes,
            &cancel,
        )?;
        match diff(
            &content.old,
            &content.new,
            BinaryMode::Auto,
            limits,
            &cancel,
        )? {
            ContentDiff::Unchanged => println!("  unchanged content (check modes/existence above)"),
            ContentDiff::Binary { old, new } => {
                println!("  binary: {} -> {} bytes", old.len(), new.len())
            }
            ContentDiff::Text(text) => {
                for edit in text.edits() {
                    println!(
                        "  old lines {:?} -> new lines {:?} (zero-based)",
                        edit.old_lines, edit.new_lines
                    );
                    println!("  - {}", escaped(&text.old_bytes()[edit.old_bytes.clone()]));
                    println!("  + {}", escaped(&text.new_bytes()[edit.new_bytes.clone()]));
                }
            }
        }
    }
    Ok(())
}

fn escaped(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| std::ascii::escape_default(*b))
        .map(char::from)
        .collect()
}
