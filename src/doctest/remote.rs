// Hidden setup shared by transfer doctests; include it with
// `include!("…/doctest/remote.rs")` as an expression inside a doctest whose body returns
// `Result<_, Box<dyn std::error::Error>>`.
//
// Evaluates to `(directory, upstream, repo, main, person)`:
//
// - `directory`: a `tempfile::TempDir` holding both repositories; dropping it removes them.
// - `upstream`: a bare SHA-1 repository at `<directory>/upstream.git` whose `refs/heads/main`
//   points to one commit with an empty tree. Its HEAD is the default symbolic `refs/heads/main`.
// - `repo`: an ordinary repository at `<directory>/local` with no commits and a remote named
//   `origin` whose URL is the relative path `../upstream.git` and whose fetch refspec is
//   `+refs/heads/*:refs/remotes/origin/*`. Without a current directory in the transfer's
//   `Environment`, relative URLs resolve against the working tree, so examples should pass
//   `Environment::default()`; that also keeps them independent of the caller's
//   `GIT_ALLOW_PROTOCOL` and other settings. A relative URL is used because every remote parser
//   accepts it on every platform, unlike a Windows drive path.
// - `main`: the ID of upstream's `main` commit.
// - `person`: the author, committer and reflog identity used throughout.
{
    let directory = tempfile::tempdir()?;
    let upstream = girt::Repository::init(
        girt::ObjectFormat::Sha1,
        directory.path().join("upstream.git"),
        girt::InitKind::Bare,
    )?;
    let person = girt::Signature {
        name: b"A. Writer".to_vec(),
        email: b"writer@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let loose = upstream.loose_objects();
    let main = loose.write_commit(&girt::Commit::new(girt::CommitFields {
        tree: loose.write_tree(&girt::Tree::new(girt::ObjectFormat::Sha1, vec![])?)?,
        parents: vec![],
        author: person.clone(),
        committer: person.clone(),
        extra_headers: vec![],
        message: b"Initial commit\n".to_vec(),
    })?)?;
    upstream.references()?.update_without_reflog(
        &girt::refs::RefName::new("refs/heads/main")?,
        girt::refs::Target::Direct(main),
        girt::refs::Expected::Absent,
    )?;
    let local_path = directory.path().join("local");
    let local = girt::Repository::init(
        girt::ObjectFormat::Sha1,
        &local_path,
        girt::InitKind::Worktree,
    )?;
    let mut edit = local.edit_config(1 << 20)?;
    girt::remote::RemoteConfig::new(edit.document_mut()).add(
        b"origin",
        b"../upstream.git",
        &[b"+refs/heads/*:refs/remotes/origin/*"],
    )?;
    edit.commit()?;
    // Reopen so the repository's configuration snapshot includes the new remote.
    let repo = girt::Repository::open(&local_path)?;
    (directory, upstream, repo, main, person)
}
