//! Publish an unborn branch and a tag, then delete the tag while retaining its reflog.
use girt::refs::{Expected, RefEdit, Reflog, TransactionError};
use girt::{Commit, CommitFields, Repository, Signature, Tree};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        directory.path().join("repository"),
        girt::InitKind::Bare,
    )?;
    let objects = repo.loose_objects();
    let tree = objects.write_tree(&Tree::new(girt::ObjectFormat::Sha1, Vec::new())?)?;
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
    let head = girt::refs::RefName::new("HEAD")?;
    let tag = girt::refs::RefName::new("refs/tags/initial")?;
    let edits = [
        RefEdit {
            name: head.clone(),
            dereference: true,
            target: Some(girt::refs::Target::Direct(id)),
            expected: Expected::Absent,
            reflog: Reflog::Append {
                committer: committer.clone(),
                message: b"initial publication".to_vec(),
            },
        },
        RefEdit {
            name: tag.clone(),
            dereference: false,
            target: Some(girt::refs::Target::Direct(id)),
            expected: Expected::Absent,
            reflog: Reflog::Append {
                committer: committer.clone(),
                message: b"mark initial".to_vec(),
            },
        },
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
    }
    refs.transaction(&[RefEdit {
        name: tag.clone(),
        dereference: false,
        target: None,
        expected: Expected::Value(girt::refs::Target::Direct(id)),
        reflog: Reflog::Append {
            committer,
            message: b"remove temporary tag".to_vec(),
        },
    }])?;
    assert_eq!(refs.reflog(&head)?.unwrap().len(), 1);
    assert_eq!(refs.reflog(&tag)?.unwrap().len(), 2);
    assert_eq!(refs.read(&tag)?, None);
    Ok(())
}
