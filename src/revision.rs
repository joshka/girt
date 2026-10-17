//! Git revision expressions: parsing and resolving names like `main~2`, `v1.0^{tree}` or
//! `HEAD:src/lib.rs`, and commit ranges like `main..topic`.
//!
//! Commands that accept a revision from a person need the same language Git's commands accept.
//! This module splits that work in two steps:
//!
//! 1. [`Revision`] and [`Range`] parse text into a description of what to look up. Parsing reads no
//!    repository state, so syntax errors surface before any I/O.
//! 2. [`Resolver`] looks a parsed expression up in one repository: references, reflogs, branch
//!    configuration and objects. It returns object IDs, or a [`Selection`] of included and excluded
//!    commits for a set of ranges.
//!
//! # Supported syntax
//!
//! | Expression | Meaning |
//! | --- | --- |
//! | full object ID | that ID; existence is not checked, as Git does not check it |
//! | abbreviated object ID | the one stored object with that prefix of at least four digits; a following `~`, `^`, `^{kind}` or `:path` picks among objects sharing it by kind |
//! | `name` | a reference, expanded in Git's order: `HEAD` or `refs/...` as given, `refs/name`, `refs/tags/name`, `refs/heads/name`, `refs/remotes/name`, `refs/remotes/name/HEAD` |
//! | `@` | `HEAD` |
//! | `rev~N`, `rev~` | the Nth first-parent ancestor |
//! | `rev^N`, `rev^` | the Nth parent; `^0` is the commit itself |
//! | `rev^{}` | `rev` with annotated tags peeled |
//! | `rev^{commit}`, `^{tree}`, `^{blob}`, `^{tag}`, `^{object}` | `rev` peeled to, or checked as, that kind |
//! | `rev:path` | the tree entry at `path` in `rev`'s tree; `rev:` is the tree itself |
//! | `name@{N}`, `@{N}` | the Nth prior value from a reflog; bare `@{N}` uses the current branch |
//! | `name@{upstream}`, `@{u}` | the remote-tracking branch configured as the branch's upstream |
//! | `name@{push}`, `@{push}` | the remote-tracking branch the branch would push to |
//! | `A..B`, `A...B`, `^A` | ranges ([`Range`]); an omitted side means `HEAD` |
//!
//! Name lookup tries references before abbreviated object IDs, as Git does, and takes the first
//! matching reference without reporting ambiguity.
//!
//! # Not yet supported
//!
//! Each of these ([#364](https://github.com/joshka/girt/issues/364)) is parsed far enough to be
//! recognised and then refused with
//! [`RevisionError::Unsupported`], never approximated:
//!
//! - pseudo-references other than `HEAD`, such as `ORIG_HEAD` and `FETCH_HEAD`, when no reference
//!   under `refs/` matches the name;
//! - index paths `:path` and `:N:path`, and message searches `:/text` and `rev^{/text}`;
//! - dated reflog entries `name@{yesterday}` and previous checkouts `@{-N}`;
//! - `rev^@`, `rev^!` and `rev^-`;
//! - `@{push}` with configured `remote.<name>.push` refspecs;
//! - `git describe` output such as `v1.0-3-gabc1234`;
//! - paths relative to the working directory (`rev:./path`).
//!
//! # Examples
//!
//! ```
//! use girt::revision::{Range, Resolver, Revision};
//! # use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
//! # use girt::{Commit, CommitFields, InitKind, ObjectFormat, PackLimits, Repository, Signature, Tree};
//! # let directory = tempfile::tempdir()?;
//! # let repo = Repository::init(ObjectFormat::Sha1, directory.path().join("r.git"), InitKind::Bare)?;
//! # let loose = repo.loose_objects();
//! # let person = Signature { name: b"A. Writer".to_vec(), email: b"a@example.com".to_vec(),
//! #                          seconds: 1_700_000_000, offset_minutes: 0 };
//! # let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
//! # let commit = |parents: Vec<_>| loose.write_commit(&Commit::new(CommitFields {
//! #     tree, parents, author: person.clone(), committer: person.clone(),
//! #     extra_headers: vec![], message: b"Snapshot\n".to_vec(),
//! # }).unwrap());
//! # let first = commit(vec![])?;
//! # let second = commit(vec![first])?;
//! # let main = RefName::new("refs/heads/main")?;
//! # repo.references()?.transaction(&[RefEdit::new(main, Some(Target::Direct(second)),
//! #     Expected::Absent, Reflog::Preserve)])?;
//! let objects = repo.objects_with(PackLimits::default())?;
//! let resolver = Resolver::new(&repo, &objects)?;
//!
//! let parent = resolver.resolve(&"main~1".parse::<Revision>()?)?;
//! assert_eq!(parent, first);
//! assert_eq!(resolver.resolve(&"main^{tree}".parse()?)?, tree);
//!
//! let range: Range = "main~1..main".parse()?;
//! let selection = resolver.select(&[range])?;
//! assert_eq!(selection.include, [second]);
//! assert_eq!(selection.exclude, [first]);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod error;
mod resolve;
mod syntax;
mod upstream;

pub use error::RevisionError;
pub use resolve::{Resolver, Selection, ref_candidates};
pub use syntax::{Base, ParseRevisionError, Peel, Range, Revision, Step};
