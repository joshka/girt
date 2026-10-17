// Hidden helper shared by transfer doctests: evaluates to a closure that writes a commit with an
// empty tree to a repository's loose storage and returns its ID. It does not move any reference.
//
// `let commit = include!("…/doctest/commit.rs");`
// `let id = commit(&repo, &[parent], "message")?;`
|repository: &girt::Repository,
 parents: &[girt::ObjectId],
 message: &str|
 -> Result<girt::ObjectId, Box<dyn std::error::Error>> {
    let person = girt::Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let loose = repository.loose_objects();
    let format = repository.object_format();
    Ok(loose.write_commit(&girt::Commit::new(girt::CommitFields {
        tree: loose.write_tree(&girt::Tree::new(format, vec![])?)?,
        parents: parents.to_vec(),
        author: person.clone(),
        committer: person,
        extra_headers: vec![],
        message: format!("{message}\n").into_bytes(),
    })?)?)
}
