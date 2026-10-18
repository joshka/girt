use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use super::{Action, Error, Kind, Limits, Recovery, RefUpdate, State};
use crate::replay::todo::Todo;
use crate::{ObjectFormat, ObjectId};
const PSEUDO: &[&str] = &[
    "CHERRY_PICK_HEAD",
    "REVERT_HEAD",
    "REBASE_HEAD",
    "ORIG_HEAD",
    "MERGE_MSG",
    "AUTO_MERGE",
];
const REBASE_FILES: &[&str] = &[
    "head-name",
    "onto",
    "orig-head",
    "git-rebase-todo",
    "git-rebase-todo.backup",
    "done",
    "author-script",
    "message",
    "msgnum",
    "end",
    "stopped-sha",
    "patch",
    "interactive",
    "no-reschedule-failed-exec",
    "reschedule-failed-exec",
    "drop_redundant_commits",
    "keep_redundant_commits",
    "keep_empty",
    "strategy",
    "strategy_opts",
    "gpg_sign_opt",
    "quiet",
    "verbose",
    "signoff",
    "rewritten-list",
    "rewritten-pending",
    "update-refs",
    "amend",
    "date_is_author_date",
    "ignore_date",
];

const AM_FILES: &[&str] = &[
    "applying",
    "rebasing",
    "abort-safety",
    "apply-opt",
    "author-script",
    "final-commit",
    "info",
    "keep",
    "last",
    "messageid",
    "msg",
    "next",
    "patch",
    "quiet",
    "quoted-cr",
    "scissors",
    "sign",
    "threeway",
    "utf8",
];

pub(super) fn am_range(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<(usize, usize), Error> {
    crate::am::mail_range(
        files.get(Path::new("rebase-apply/next")).map(Vec::as_slice),
        files.get(Path::new("rebase-apply/last")).map(Vec::as_slice),
    )
    .map_err(|_| Error::Unsupported(PathBuf::from("rebase-apply/next")))
}

fn validate_am(files: &BTreeMap<PathBuf, Vec<u8>>) -> Result<(), Error> {
    if !files.contains_key(Path::new("rebase-apply/applying"))
        || files.contains_key(Path::new("rebase-apply/rebasing"))
    {
        return Err(Error::Unsupported(PathBuf::from("rebase-apply")));
    }
    let (next, last) = am_range(files)?;
    if next <= last && !files.contains_key(&PathBuf::from(format!("rebase-apply/{next:04}"))) {
        return Err(Error::Unsupported(PathBuf::from(format!(
            "rebase-apply/{next:04}"
        ))));
    }
    for name in files
        .keys()
        .filter_map(|path| path.strip_prefix("rebase-apply").ok())
    {
        if let Some(text) = name
            .to_str()
            .filter(|text| text.as_bytes().iter().all(u8::is_ascii_digit))
        {
            let number: usize = text
                .parse()
                .map_err(|_| Error::Unsupported(name.to_path_buf()))?;
            if number == 0 || number > last || text != format!("{number:04}") {
                return Err(Error::Unsupported(PathBuf::from("rebase-apply").join(name)));
            }
        }
    }
    Ok(())
}

impl State {
    /// Reads am, merge-backend rebase, sequencer directories or single-operation pseudo-refs.
    ///
    /// Uses the worktree-specific Git directory, not the common object/ref directory. Unknown
    /// files, symlinks, apply-backend rebase, simultaneous incompatible operations and malformed
    /// object identities are refused. Original bytes, including author scripts, remain data.
    /// This snapshot requires a caller-owned trusted directory and exclusive operation ownership
    /// during observation/publication; it is not a lock against an independently running Git.
    /// Use [`Self::read_repository`] for am recovery across files/reftable ref backends.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for malformed/unsupported state, I/O, todo syntax or bounds.
    pub fn read(
        git_dir: impl AsRef<Path>,
        format: ObjectFormat,
        limits: Limits,
    ) -> Result<Option<Self>, Error> {
        let git_dir = git_dir.as_ref().to_path_buf();
        let files = snapshot(&git_dir, limits)?;
        let rebase = git_dir.join("rebase-merge").is_dir();
        let sequence = git_dir.join("sequencer").is_dir();
        let am = git_dir.join("rebase-apply").is_dir();
        if files.contains_key(Path::new("REBASE_HEAD")) && !rebase {
            return Err(Error::Unsupported(git_dir.join("REBASE_HEAD")));
        }
        let cherry = files.contains_key(Path::new("CHERRY_PICK_HEAD"));
        let revert = files.contains_key(Path::new("REVERT_HEAD"));
        if (rebase && sequence)
            || (cherry && revert)
            || (am && (rebase || sequence || cherry || revert))
        {
            return Err(Error::Unsupported(git_dir));
        }
        if am {
            validate_am(&files)?;
        }
        let kind = if am {
            Kind::Am
        } else if rebase {
            Kind::Rebase
        } else if cherry {
            Kind::CherryPick
        } else if revert {
            Kind::Revert
        } else if sequence {
            let todo = files
                .get(Path::new("sequencer/todo"))
                .ok_or_else(|| Error::Unsupported(git_dir.join("sequencer/todo")))?;
            let parsed = Todo::parse(todo, limits.todo)?;
            parsed
                .lines()
                .iter()
                .find_map(|line| match line.instruction {
                    crate::replay::todo::Instruction::Pick(_) => Some(Kind::CherryPick),
                    crate::replay::todo::Instruction::Revert(_) => Some(Kind::Revert),
                    _ => None,
                })
                .ok_or_else(|| Error::Unsupported(git_dir.join("sequencer/todo")))?
        } else {
            return Ok(None);
        };
        let directory = if am {
            Some(PathBuf::from("rebase-apply"))
        } else if rebase {
            Some(PathBuf::from("rebase-merge"))
        } else if sequence {
            Some(PathBuf::from("sequencer"))
        } else {
            None
        };
        let read_id = |name: &str| parse_optional_id(&files, name, format);
        let pending = match kind {
            Kind::Am => None,
            Kind::CherryPick => read_id("CHERRY_PICK_HEAD")?,
            Kind::Revert => read_id("REVERT_HEAD")?,
            Kind::Rebase => {
                let head = read_id("REBASE_HEAD")?;
                let stopped = read_id("rebase-merge/stopped-sha")?;
                if matches!((head, stopped), (Some(a), Some(b)) if a != b) {
                    return Err(Error::Unsupported(git_dir.join("REBASE_HEAD")));
                }
                head.or(stopped)
            }
        };
        let original_head = if am {
            read_id("ORIG_HEAD")?
        } else if rebase {
            read_id("rebase-merge/orig-head")?
        } else {
            read_id("sequencer/head")?
        };
        let onto = read_id("rebase-merge/onto")?;
        let branch = files
            .get(Path::new("rebase-merge/head-name"))
            .map(|bytes| {
                let name = bytes.trim_ascii();
                if name == b"detached HEAD" {
                    Ok(None)
                } else {
                    if !name.starts_with(b"refs/heads/") {
                        return Err(Error::Unsupported(PathBuf::from("rebase-merge/head-name")));
                    }
                    crate::refs::RefName::new(name)
                        .map(Some)
                        .map_err(|_| Error::Unsupported(PathBuf::from("rebase-merge/head-name")))
                }
            })
            .transpose()?
            .flatten();
        let todo_path = if rebase {
            "rebase-merge/git-rebase-todo"
        } else {
            "sequencer/todo"
        };
        let todo = files
            .get(Path::new(todo_path))
            .map(|bytes| Todo::parse(bytes, limits.todo))
            .transpose()?;
        if sequence && (original_head.is_none() || todo.is_none()) {
            return Err(Error::Unsupported(git_dir.join("sequencer")));
        }
        if rebase
            && (original_head.is_none()
                || onto.is_none()
                || todo.is_none()
                || !files.contains_key(Path::new("rebase-merge/head-name")))
        {
            return Err(Error::Unsupported(git_dir.join("rebase-merge")));
        }
        if sequence
            && let (Some(source), Some(first)) =
                (pending, todo.as_ref().and_then(|todo| todo.lines().first()))
        {
            let token = match (&first.instruction, kind) {
                (crate::replay::todo::Instruction::Pick(token), Kind::CherryPick)
                | (crate::replay::todo::Instruction::Revert(token), Kind::Revert) => token,
                _ => return Err(Error::Unsupported(PathBuf::from("sequencer/todo"))),
            };
            if !source.to_string().as_bytes().starts_with(&token.id) {
                return Err(Error::Unsupported(PathBuf::from("sequencer/todo")));
            }
        }

        Ok(Some(Self {
            git_dir,
            format,
            limits,
            files,
            kind,
            directory,
            pending,
            original_head,
            onto,
            branch,
            todo,
        }))
    }

    /// Reads operation files and am's original HEAD through the repository's ref backend.
    ///
    /// Use this for portable am recovery: reftable stores `ORIG_HEAD` outside loose metadata
    /// files. Publication verifies the observed original identity as well as the file snapshot.
    /// Caller-owned operation/ref writer exclusion is required through observation and publication.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for state admission or reference observation failures.
    pub fn read_repository(
        repository: &crate::Repository,
        limits: Limits,
    ) -> Result<Option<Self>, Error> {
        let Some(mut state) = Self::read(repository.git_dir(), repository.object_format(), limits)?
        else {
            return Ok(None);
        };
        if state.kind == Kind::Am {
            state.original_head = am_original_head(repository, limits)?;
        }
        Ok(Some(state))
    }

    /// Operation kind identified by the observed layout.
    pub fn kind(&self) -> Kind {
        self.kind
    }
    /// Current unresolved source, if Git recorded one. Am mail has no source object here.
    pub fn pending(&self) -> Option<ObjectId> {
        self.pending
    }
    /// Original operation tip from sequence metadata or am's original-HEAD ref.
    pub fn original_head(&self) -> Option<ObjectId> {
        self.original_head
    }
    /// Rebase destination recorded by Git.
    pub fn onto(&self) -> Option<ObjectId> {
        self.onto
    }
    /// Remaining parsed instructions; absent for am. Rebase's stopped instruction is in `done`.
    pub fn todo(&self) -> Option<&Todo> {
        self.todo.as_ref()
    }
    /// Borrows an observed state file by its repository-relative metadata path.
    pub fn file(&self, path: impl AsRef<Path>) -> Option<&[u8]> {
        self.files.get(path.as_ref()).map(Vec::as_slice)
    }

    /// Reads conditional branch targets from Git's three-line update-refs records.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for malformed names, object identities or incomplete records.
    pub fn ref_updates(&self) -> Result<Vec<RefUpdate>, Error> {
        let Some(bytes) = self.file("rebase-merge/update-refs") else {
            return Ok(Vec::new());
        };
        if bytes.is_empty() {
            return Ok(Vec::new());
        }
        let lines: Vec<_> = bytes
            .strip_suffix(b"\n")
            .unwrap_or(bytes)
            .split(|&b| b == b'\n')
            .collect();
        if lines.len() % 3 != 0 {
            return Err(Error::Unsupported(PathBuf::from(
                "rebase-merge/update-refs",
            )));
        }
        let invalid = || Error::Unsupported(PathBuf::from("rebase-merge/update-refs"));
        let mut updates = Vec::new();
        for record in lines.as_chunks::<3>().0 {
            let name = crate::refs::RefName::new(record[0]).map_err(|_| invalid())?;
            if !name.as_bytes().starts_with(b"refs/heads/") {
                return Err(invalid());
            }
            let old = ObjectId::from_hex(
                self.format,
                std::str::from_utf8(record[1]).map_err(|_| invalid())?,
            )
            .map_err(|_| invalid())?;
            let new = ObjectId::from_hex(
                self.format,
                std::str::from_utf8(record[2]).map_err(|_| invalid())?,
            )
            .map_err(|_| invalid())?;
            let new = if record[2].iter().all(|&b| b == b'0') {
                None
            } else {
                Some(new)
            };
            updates.push(RefUpdate { name, old, new });
        }
        Ok(updates)
    }

    /// Prepares metadata changes after an explicit continue, skip or abort decision.
    ///
    /// `head` is the caller's observed current HEAD. Continue names an already computed resolved
    /// commit; this method does not validate its tree or publish it. Before metadata publication,
    /// install objects and perform the required guarded checkout and expected-value HEAD update.
    /// For rebase continuation, leave remaining todo execution/final branch reattachment to Git or
    /// the caller's executor. Squash/fixup continuation metadata is explicitly unsupported.
    /// Am continue/skip require the final numbered mail; abort requires its original HEAD and a
    /// matching abort-safety checkpoint. Queued-mail execution is left to a later executor.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for unsupported flags/topology, absent abort identity, stale abort-safety
    /// or malformed state. Preparing the plan changes no repository data.
    pub fn prepare(&self, action: Action, head: ObjectId) -> Result<Recovery, Error> {
        self.prepare_inner(action, Some(head), false)
    }

    pub(super) fn prepare_inner(
        &self,
        action: Action,
        head: Option<ObjectId>,
        queued_am: bool,
    ) -> Result<Recovery, Error> {
        let unborn = head.is_none();
        let head = head.unwrap_or_else(|| ObjectId::null(self.format));
        if head.format() != self.format
            || matches!(action, Action::Continue(id) if id.format() != self.format)
        {
            return Err(Error::Unsupported(PathBuf::from("HEAD")));
        }
        if self.kind == Kind::Am {
            let (next, last) = am_range(&self.files)?;
            if matches!(action, Action::Continue(_) | Action::Skip) && next != last && !queued_am {
                return Err(Error::Unsupported(PathBuf::from("rebase-apply/next")));
            }
            if matches!(action, Action::Abort)
                && !unborn
                && self.original_head.is_none_or(|id| id.is_null())
            {
                return Err(Error::Unsupported(PathBuf::from("ORIG_HEAD")));
            }
            if unborn {
                if self
                    .files
                    .get(Path::new("rebase-apply/abort-safety"))
                    .map(Vec::as_slice)
                    != Some(b"".as_slice())
                {
                    return Err(Error::HeadChanged);
                }
            } else {
                let safe =
                    parse_optional_id(&self.files, "rebase-apply/abort-safety", self.format)?
                        .ok_or_else(|| {
                            Error::Unsupported(PathBuf::from("rebase-apply/abort-safety"))
                        })?;
                if safe != head {
                    return Err(Error::HeadChanged);
                }
            }
        }
        if self.files.contains_key(Path::new("sequencer/opts")) {
            return Err(Error::Unsupported(PathBuf::from("sequencer/opts")));
        }
        if self.files.contains_key(Path::new("rebase-merge/amend")) {
            return Err(Error::Unsupported(PathBuf::from("rebase-merge/amend")));
        }
        if let Some(todo) = &self.todo
            && todo.lines().iter().any(|line| {
                matches!(
                    line.instruction,
                    crate::replay::todo::Instruction::Merge { .. }
                        | crate::replay::todo::Instruction::Reset(_)
                )
            })
        {
            return Err(Error::Unsupported(
                self.directory.clone().unwrap_or_default(),
            ));
        }

        if matches!(action, Action::Finish)
            && (self.kind != Kind::Rebase
                || self.pending.is_some()
                || self
                    .todo
                    .as_ref()
                    .is_some_and(|todo| !todo.lines().is_empty()))
        {
            return Err(Error::Unsupported(PathBuf::from("rebase-merge")));
        }
        let reference_updates = if matches!(action, Action::Finish) {
            self.ref_updates()?
        } else {
            Vec::new()
        };
        if reference_updates.iter().any(|update| update.new.is_none()) {
            return Err(Error::Unsupported(PathBuf::from(
                "rebase-merge/update-refs",
            )));
        }
        let mut after = self.files.clone();
        let destination = match action {
            Action::Continue(id) => id,
            Action::Skip | Action::Finish => head,
            Action::Abort => self.original_head.unwrap_or(head),
        };
        let abort = matches!(action, Action::Abort);
        let finish = matches!(action, Action::Finish);
        if abort || finish {
            if let Some(safe) =
                parse_optional_id(&self.files, "sequencer/abort-safety", self.format)?
                && safe != head
                && self.original_head != Some(head)
            {
                return Err(Error::HeadChanged);
            }
            if let Some(directory) = &self.directory {
                after.retain(|path, _| !path.starts_with(directory));
            }
        } else if self.kind == Kind::Am {
            after.retain(|path, _| !path.starts_with("rebase-apply"));
        } else if self.kind == Kind::Rebase {
            for name in ["author-script", "message", "patch", "stopped-sha"] {
                after.remove(&PathBuf::from("rebase-merge").join(name));
            }
            if let (Action::Continue(id), Some(source)) = (action, self.pending) {
                let rewritten = after
                    .entry(PathBuf::from("rebase-merge/rewritten-list"))
                    .or_default();
                rewritten.extend_from_slice(format!("{source} {id}\n").as_bytes());
            }
        } else if self.directory.is_some() {
            let path = PathBuf::from("sequencer/todo");
            let bytes = after
                .get(&path)
                .ok_or_else(|| Error::Unsupported(path.clone()))?;
            let todo = Todo::parse(bytes, self.limits.todo)?;
            let remaining = todo
                .lines()
                .first()
                .map(|line| remove_line(bytes, line.line))
                .unwrap_or_default();
            let remaining_todo = Todo::parse(&remaining, self.limits.todo)?;
            if remaining_todo.lines().is_empty() {
                after.retain(|path, _| !path.starts_with("sequencer"));
            } else {
                after.insert(path, remaining);
                after.insert(
                    PathBuf::from("sequencer/abort-safety"),
                    format!("{destination}\n").into_bytes(),
                );
            }
        }
        for name in [
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "REBASE_HEAD",
            "MERGE_MSG",
            "AUTO_MERGE",
        ] {
            after.remove(Path::new(name));
        }
        let reflog_message = match (self.kind, action) {
            (Kind::Am, Action::Continue(_)) => b"am".to_vec(),
            (Kind::Am, Action::Skip) => vec![],
            (Kind::Am, Action::Abort) => b"am --abort".to_vec(),
            (Kind::Rebase, Action::Abort) => {
                rebase_return_message(b"rebase (abort): returning to ", self.branch.as_ref())
            }
            (Kind::Rebase, Action::Finish) => {
                rebase_return_message(b"rebase (finish): returning to ", self.branch.as_ref())
            }
            (Kind::Rebase, Action::Continue(_)) => b"rebase (continue)".to_vec(),
            (Kind::Rebase, Action::Skip) => b"rebase (skip)".to_vec(),
            (Kind::CherryPick, Action::Continue(_)) => b"commit (cherry-pick)".to_vec(),
            (Kind::Revert, Action::Continue(_)) => b"commit".to_vec(),
            (_, Action::Abort) => b"reset: moving to original HEAD".to_vec(),
            (_, Action::Skip) => b"reset: moving to HEAD".to_vec(),
            (_, Action::Finish) => return Err(Error::Unsupported(PathBuf::from("sequencer"))),
        };
        Ok(Recovery {
            state: self.clone(),
            after,
            destination,
            restore_branch: if abort || finish {
                self.branch.clone()
            } else {
                None
            },
            expected_branch_tip: if finish { self.original_head } else { None },
            reference_updates,
            reflog_message,
        })
    }
}

pub(super) fn am_original_head(
    repository: &crate::Repository,
    limits: Limits,
) -> Result<Option<ObjectId>, Error> {
    if repository.reference_backend() == crate::refs::Backend::Files {
        let mut files = BTreeMap::new();
        let mut bytes = 0;
        read_file(
            repository.git_dir(),
            Path::new("ORIG_HEAD"),
            limits,
            &mut bytes,
            &mut files,
        )?;
        return parse_optional_id(&files, "ORIG_HEAD", repository.object_format());
    }
    let snapshot = crate::refs::reftable::Snapshot::read(
        &repository.git_dir().join("reftable"),
        repository.object_format(),
        limits.reftable,
        &std::sync::atomic::AtomicBool::new(false),
    )?;
    let target = snapshot
        .table
        .references
        .into_iter()
        .find(|record| record.name.as_ref() == b"ORIG_HEAD")
        .and_then(|record| record.target);
    match target {
        Some(crate::refs::Target::Direct(id)) => Ok(Some(id)),
        None => Ok(None),
        Some(crate::refs::Target::Symbolic(_)) => {
            Err(Error::Unsupported(PathBuf::from("ORIG_HEAD")))
        }
    }
}

fn remove_line(bytes: &[u8], line: usize) -> Vec<u8> {
    bytes
        .split_inclusive(|&b| b == b'\n')
        .enumerate()
        .filter(|(index, _)| *index + 1 != line)
        .flat_map(|(_, row)| row.iter().copied())
        .collect()
}
fn parse_optional_id(
    files: &BTreeMap<PathBuf, Vec<u8>>,
    name: &str,
    format: ObjectFormat,
) -> Result<Option<ObjectId>, Error> {
    files
        .get(Path::new(name))
        .map(|bytes| {
            let text = std::str::from_utf8(bytes.trim_ascii())
                .map_err(|_| Error::Unsupported(PathBuf::from(name)))?;
            ObjectId::from_hex(format, text).map_err(|_| Error::Unsupported(PathBuf::from(name)))
        })
        .transpose()
}
fn io_error(path: impl Into<PathBuf>, source: io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

pub(super) fn snapshot(
    git_dir: &Path,
    limits: Limits,
) -> Result<BTreeMap<PathBuf, Vec<u8>>, Error> {
    let mut files = BTreeMap::new();
    if fs::symlink_metadata(git_dir.join("MERGE_HEAD")).is_ok() {
        return Err(Error::Unsupported(git_dir.join("MERGE_HEAD")));
    }
    let mut bytes = 0usize;
    for name in PSEUDO {
        read_file(git_dir, Path::new(name), limits, &mut bytes, &mut files)?;
    }
    for directory in ["sequencer", "rebase-merge", "rebase-apply"] {
        let path = git_dir.join(directory);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&path, error)),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Unsupported(path));
        }
        for entry in fs::read_dir(&path).map_err(|error| io_error(&path, error))? {
            let entry = entry.map_err(|error| io_error(&path, error))?;
            let name = entry.file_name();
            let allowed = name.to_str().is_some_and(|name| {
                if directory == "sequencer" {
                    ["head", "todo", "abort-safety", "opts"].contains(&name)
                } else if directory == "rebase-apply" {
                    AM_FILES.contains(&name)
                        || name.len() >= 4 && name.as_bytes().iter().all(u8::is_ascii_digit)
                } else {
                    REBASE_FILES.contains(&name)
                }
            });
            if !allowed {
                return Err(Error::Unsupported(entry.path()));
            }
            read_file(
                git_dir,
                &PathBuf::from(directory).join(name),
                limits,
                &mut bytes,
                &mut files,
            )?;
        }
    }
    Ok(files)
}
fn read_file(
    git_dir: &Path,
    name: &Path,
    limits: Limits,
    total: &mut usize,
    files: &mut BTreeMap<PathBuf, Vec<u8>>,
) -> Result<(), Error> {
    use std::io::Read;
    let path = git_dir.join(name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(&path, error)),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(Error::Unsupported(path));
    }
    if files.len() >= limits.max_files
        || metadata.len() > limits.max_bytes.saturating_sub(*total) as u64
    {
        return Err(Error::Limit);
    }
    let mut payload = Vec::new();
    fs::File::open(&path)
        .map_err(|error| io_error(&path, error))?
        .take((limits.max_bytes.saturating_sub(*total) as u64).saturating_add(1))
        .read_to_end(&mut payload)
        .map_err(|error| io_error(&path, error))?;
    *total = total.checked_add(payload.len()).ok_or(Error::Limit)?;
    if *total > limits.max_bytes {
        return Err(Error::Limit);
    }
    files.insert(name.to_path_buf(), payload);
    Ok(())
}

fn rebase_return_message(prefix: &[u8], branch: Option<&crate::refs::RefName>) -> Vec<u8> {
    let mut message = prefix.to_vec();
    message.extend_from_slice(
        branch
            .map(|name| name.as_bytes())
            .unwrap_or(b"detached HEAD"),
    );
    message
}
