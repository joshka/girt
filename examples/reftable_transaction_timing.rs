//! Times a single reftable transaction on an existing Git-generated repository.
//!
//! Run with a repository path and edit count (for example, 1 or 200). Fixture creation and ref
//! listing comparisons belong outside the timed process. Repeated runs replace the same refs.
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::refs::{Backend, Expected, RefEdit, RefName, Reflog, Target};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("expected repository path")?;
    let count: usize = args.next().ok_or("expected edit count")?.parse()?;
    let controlled = args.next().as_deref() == Some("controlled");
    let repo = Repository::open(path)?;
    assert_eq!(repo.reference_backend(), Backend::Reftable);
    let refs = repo.references()?;
    let id = refs
        .resolve_with(&RefName::HEAD, 32)?
        .id
        .ok_or("unborn HEAD")?;
    let edits: Vec<_> = (0..count)
        .map(|index| {
            Ok(RefEdit::new(
                RefName::new(format!("refs/tags/timing-{index:06}"))?,
                Some(Target::Direct(id)),
                Expected::Any,
                Reflog::Preserve,
            ))
        })
        .collect::<Result<_, girt::refs::InvalidRefName>>()?;
    if controlled {
        refs.transaction_controlled(&edits, &AtomicBool::new(false))?;
    } else {
        refs.transaction(&edits)?;
    }
    Ok(())
}
