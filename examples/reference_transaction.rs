//! Publish an unborn branch and a tag, then delete the tag while retaining its reflog.
use girt::refs::{Expected, RefEdit, RefName, Reflog, TransactionError};
use girt::{Commit, CommitFields, Repository, Signature};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        girt::ObjectFormat::Sha256,
        directory.path().join("repository"),
        girt::InitKind::Bare,
    )?;

    let objects = repo.loose_objects();
    let tree = objects.write_tree(&repo.new_tree(Vec::new())?)?;

    let committer = Signature {
        name: b"Example Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1700000000,
        offset_minutes: 0,
    };

    let commit = Commit::new(CommitFields {
        tree,
        parents: Vec::new(),
        author: committer.clone(),
        committer: committer.clone(),
        extra_headers: Vec::new(),
        message: b"Initial commit\n".to_vec(),
    })?;
    let id = objects.write_commit(&commit)?;

    let head = RefName::HEAD;
    let tag = RefName::tag("initial")?;
    let edits = [
        {
            let input = RefEdit::new(
                head.clone(),
                Some(girt::refs::Target::Direct(id)),
                Expected::Absent,
                Reflog::Append {
                    committer: committer.clone(),
                    message: b"initial publication".to_vec(),
                },
            );
            input.with_dereference(girt::refs::Dereference::Follow)
        },
        RefEdit::create(
            tag.clone(),
            id,
            Reflog::Append {
                committer: committer.clone(),
                message: b"mark initial".to_vec(),
            },
        ),
    ];

    let refs = repo.references()?;
    match refs.transaction(&edits) {
        Ok(outcomes) => println!("Published: {outcomes:?}"),
        Err(TransactionError::Prepare { operation, source }) => {
            eprintln!("No refs or logs changed; operation {operation:?}: {source}");
            return Err(source.into());
        }
        Err(error @ TransactionError::Publish { .. }) => {
            // Inspect per-ref and per-log outcomes before deciding whether/how to retry.
            eprintln!("Partial publication: {error:?}");
            return Err(error.into());
        }
        Err(error) => return Err(error.into()),
    }

    refs.transaction(&[RefEdit::delete(
        tag.clone(),
        id,
        Reflog::Append {
            committer,
            message: b"remove temporary tag".to_vec(),
        },
    )])?;

    assert_eq!(refs.reflog(&head)?.unwrap().len(), 1);
    assert_eq!(refs.reflog(&tag)?.unwrap().len(), 2);
    assert_eq!(refs.read(&tag)?, None);

    // A retention ref may already exist at this value, but must never overwrite another value.
    let keep = RefName::new("refs/jj/keep-example")?;
    refs.transaction(&[RefEdit::new(
        keep.clone(),
        Some(girt::refs::Target::Direct(id)),
        Expected::AbsentOr(girt::refs::Target::Direct(id)),
        Reflog::Preserve,
    )])?;
    refs.transaction(&[RefEdit::new(keep, None, Expected::Exists, Reflog::Delete)])?;
    Ok(())
}
