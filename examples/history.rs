//! Run `cargo run --example history -- /path/to/repo <commit-id> [other-commit-id]`.
use girt::{HistoryLimits, ObjectId, PackLimits, Repository};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected repository path")?;
    let root: ObjectId = args.next().ok_or("expected commit ID")?.parse()?;
    let repo = Repository::open(path)?;
    let objects = repo.objects(PackLimits::default())?;
    for id in objects.walk(&[root], HistoryLimits::default())? {
        println!("{id}");
    }
    if let Some(other) = args.next() {
        let other = other.parse()?;
        println!(
            "ancestor: {}",
            objects.is_ancestor(root, other, HistoryLimits::default())?
        );
        println!(
            "merge bases: {:?}",
            objects.merge_bases(root, other, HistoryLimits::default())?
        );
    }
    Ok(())
}
