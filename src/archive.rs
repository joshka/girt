//! Export a tree as a tar or zip archive, as `git archive` does.
//!
//! [`Repository::archive`](crate::Repository::archive) writes the files of a commit or tree,
//! limited by a pathspec, in Git's layout:
//!
//! - **tar** is byte-identical to `git archive --format=tar`: POSIX ustar entries owned by `root`,
//!   a pax global header with the commit ID (read back by `git get-tar-commit-id`), pax `path` and
//!   `linkpath` records for overlong names, and padding to 10240-byte records.
//! - **tar.gz** compresses that stream with deflate at Git's default level and a gzip header with
//!   no name and no time, as Git's built-in `git archive gzip` does.
//! - **zip** stores or deflates each file, records Unix modes for executables and symbolic links,
//!   the commit time in an extended-timestamp field and in DOS fields in the local time zone, and
//!   the commit ID as the archive comment.
//!
//! Entry times are the committer time of a commit, or the current time for a bare tree, unless
//! [`Options::with_mtime`] sets one. Directories, files and executables get mode `0777`, `0666`
//! and `0777` less `tar.umask` (default `002`); symbolic links keep `0777`, and submodules become
//! empty directories.
//!
//! Attributes come from the archived tree (or the working tree with
//! [`Options::with_worktree_attributes`]): `export-ignore` leaves a path out, `export-subst`
//! expands `$Format:…$` with a commit's [placeholders](Options#export-subst), and the filters
//! pipeline converts content as a checkout writes it: `ident`, line endings,
//! `working-tree-encoding`, and smudge commands when [`Options::with_commands`] allows them.

use std::io::{self, Write};

use flate2::Compression;
use flate2::write::DeflateEncoder;

use crate::attributes::{DirectorySource, Lookup, State};
use crate::filter::{Commands, Pipeline};
use crate::pathspec::{Pathspec, Prune};
use crate::workflow::Environment;
use crate::{Commit, EntryMode, ObjectId, ObjectKind, Objects, Repository};

mod subst;
mod tar;
#[cfg(test)]
mod tests;
mod zip;

/// Git's default compression level for zip entries and tar.gz.
const DEFAULT_LEVEL: u32 = 6;

/// The archive format.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Format {
    /// An uncompressed tar archive.
    #[default]
    Tar,
    /// A gzip-compressed tar archive (`tar.gz` or `tgz`).
    TarGz,
    /// A zip archive.
    Zip,
}

/// A file added to the archive after the tree's entries (`--add-file`, `--add-virtual-file`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtraFile {
    path: Vec<u8>,
    content: Vec<u8>,
    executable: bool,
}

impl ExtraFile {
    /// A regular file at `path` in the archive, which no prefix changes.
    pub fn new(path: impl Into<Vec<u8>>, content: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            content: content.into(),
            executable: false,
        }
    }

    /// Marks the file executable.
    #[must_use]
    pub fn with_executable(mut self, executable: bool) -> Self {
        self.executable = executable;
        self
    }
}

/// How an archive is written.
///
/// # export-subst
///
/// `$Format:…$` takes `git log --format` placeholders. girt expands `%H`, `%h`, `%T`, `%t`, `%P`,
/// `%p`, `%s`, `%B`, `%n`, `%%`, and for the author (`%a…`) and committer (`%c…`) the name
/// (`n`), email (`e`), default date (`d`), ISO date (`i`) and Unix time (`t`). Any other
/// placeholder fails with [`Error::Placeholder`] rather than writing something Git would not.
/// Abbreviations use `core.abbrev`, or Git's automatic length.
#[derive(Clone, Debug, Default)]
pub struct Options {
    format: Format,
    level: Option<u32>,
    prefix: Vec<u8>,
    subdirectory: Vec<u8>,
    mtime: Option<i64>,
    extra: Vec<ExtraFile>,
    worktree_attributes: bool,
    commands: Commands,
    environment: Environment,
}

impl Options {
    /// A tar archive with no prefix, refusing filter commands.
    pub fn new() -> Self {
        Self::default()
    }

    /// Chooses the format.
    #[must_use]
    pub fn with_format(mut self, format: Format) -> Self {
        self.format = format;
        self
    }

    /// Sets the compression level, 0 (store) to 9, for zip and tar.gz; Git's default is 6.
    #[must_use]
    pub fn with_level(mut self, level: Option<u32>) -> Self {
        self.level = level.map(|level| level.min(9));
        self
    }

    /// Prepends `prefix` to every path from the tree (`--prefix`). A prefix ending in `/` also
    /// adds that directory as an entry.
    #[must_use]
    pub fn with_prefix(mut self, prefix: impl Into<Vec<u8>>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Archives only the directory at `path` in the tree, with paths relative to it, as
    /// `git archive` does when run in a subdirectory. A commit still supplies the commit ID and
    /// times, and attributes still apply to full paths.
    #[must_use]
    pub fn with_subdirectory(mut self, path: impl Into<Vec<u8>>) -> Self {
        self.subdirectory = path.into();
        self
    }

    /// Uses this time, in seconds since the Unix epoch, for every entry (`--mtime`).
    #[must_use]
    pub fn with_mtime(mut self, mtime: Option<i64>) -> Self {
        self.mtime = mtime;
        self
    }

    /// Adds a file after the tree's entries.
    #[must_use]
    pub fn with_extra_file(mut self, file: ExtraFile) -> Self {
        self.extra.push(file);
        self
    }

    /// Reads attributes from the working tree's `.gitattributes` files instead of the archived
    /// tree's (`--worktree-attributes`).
    #[must_use]
    pub fn with_worktree_attributes(mut self, worktree_attributes: bool) -> Self {
        self.worktree_attributes = worktree_attributes;
        self
    }

    /// Decides whether smudge filter commands may run while converting content.
    #[must_use]
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = commands;
        self
    }

    /// Uses `environment` for attribute files and filter settings, as
    /// [`Repository::filters`] reads it.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }
}

/// An archive could not be written.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The archived object is not a commit, tag or tree.
    #[error("{0} is not a tree-ish")]
    NotTree(ObjectId),
    /// An object could not be read.
    #[error("reading objects")]
    Object(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A commit could not be parsed.
    #[error("reading the archived commit")]
    Commit(#[source] Box<crate::CommitError>),
    /// Attribute files or configuration could not be read.
    #[error("reading attributes")]
    Attributes(#[source] Box<crate::attributes::LoadError>),
    /// Content conversion failed for a path.
    #[error("converting {}", crate::display::quote_path(.path))]
    Filter {
        /// Repository-relative path.
        path: Vec<u8>,
        /// Conversion failure.
        #[source]
        source: Box<crate::filter::Error>,
    },
    /// An `export-subst` file uses a placeholder girt does not expand.
    #[error("unsupported export-subst placeholder {placeholder:?} in {}", crate::display::quote_path(.path))]
    Placeholder {
        /// Repository-relative path.
        path: Vec<u8>,
        /// The placeholder as written.
        placeholder: String,
    },
    /// `tar.umask` is not an octal number.
    #[error("invalid tar.umask")]
    Umask,
    /// The pathspec selects no path.
    #[error("pathspec did not match any files")]
    NoMatch,
    /// A zip archive would need zip64 records, which girt does not write.
    #[error("archive exceeds the zip format's size or entry limits")]
    TooLarge,
    /// The output could not be written.
    #[error("writing archive")]
    Write(#[from] io::Error),
}

/// One archived path.
struct Entry {
    path: Vec<u8>,
    mode: EntryMode,
    id: ObjectId,
}

impl Repository {
    /// Writes the files of `id` (a commit, a tag of one, or a tree) that `pathspec` selects as an
    /// archive to `out`, in tree order followed by [`Options::with_extra_file`] files.
    ///
    /// Blobs are read, converted and written one at a time; a zip archive also keeps one
    /// central-directory record per entry.
    ///
    /// ```
    /// use girt::archive::Options;
    /// use girt::pathspec::Pathspec;
    /// use girt::tree::TreeEditor;
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("repo.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repo.loose_objects();
    /// let mut editor = TreeEditor::new(&loose);
    /// editor.set(b"README", loose.write_blob(b"hello\n")?)?;
    /// let tree = editor.write()?;
    ///
    /// let mut tar = Vec::new();
    /// let options = Options::new().with_prefix("project/").with_mtime(Some(0));
    /// repo.archive(tree, &Pathspec::all(), &options, &mut tar)?;
    /// assert_eq!(tar.len(), 10240);
    /// assert!(tar.starts_with(b"project/\0"));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::NotTree`] for another kind of object, [`Error::NoMatch`] when a pathspec
    /// selects nothing, [`Error::Placeholder`] for an unsupported `export-subst` placeholder,
    /// [`Error::Filter`] when conversion fails (including a smudge command under
    /// [`Commands::Refuse`]), [`Error::TooLarge`] for a zip that needs zip64, and the errors of
    /// reading objects, attributes and writing `out`.
    pub fn archive(
        &self,
        id: ObjectId,
        pathspec: &Pathspec,
        options: &Options,
        out: impl Write,
    ) -> Result<(), Error> {
        let objects = self.objects().map_err(object)?;
        let (tree, commit) = peel(&objects, id)?;
        let umask = umask(self)?;
        let mtime = match (options.mtime, &commit) {
            (Some(mtime), _) => mtime,
            (None, Some((_, commit))) => commit
                .committer()
                .map_err(|source| Error::Commit(Box::new(source)))?
                .map(|identity| identity.date().map(|date| date.seconds))
                .transpose()
                .map_err(|source| Error::Commit(Box::new(source)))?
                .unwrap_or(0),
            (None, None) => std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
                }),
        };
        let source = if options.worktree_attributes {
            DirectorySource::Worktree
        } else {
            DirectorySource::Tree(tree)
        };
        let mut filters = self
            .filters_with_source(&options.environment, source, options.commands)
            .map_err(|source| Error::Attributes(Box::new(source)))?;
        let mut lookup = self
            .attributes(&options.environment, source)
            .map_err(|source| Error::Attributes(Box::new(source)))?;
        let mut entries = Vec::new();
        let mut base = options.subdirectory.clone();
        let start = subtree(&objects, tree, &base)?;
        if !base.is_empty() {
            base.push(b'/');
        }
        walk(
            &objects,
            start,
            &mut Walk {
                base: &base,
                directory: Vec::new(),
                pathspec,
                lookup: &mut lookup,
            },
            &mut entries,
        )?;
        if !pathspec.is_all() && entries.is_empty() {
            return Err(Error::NoMatch);
        }
        let subst = commit.as_ref().map(|(id, commit)| subst::Subst {
            objects: &objects,
            id: *id,
            commit,
            abbrev: abbrev_len(self, &objects),
        });
        let mut writer = match options.format {
            Format::Tar => Writer::Tar(tar::TarWriter::new(
                Output::Plain(out),
                u64::try_from(mtime).unwrap_or(0),
            )),
            Format::TarGz => Writer::Tar(tar::TarWriter::new(
                Output::Gzip(Gzip::new(out, options.level.unwrap_or(DEFAULT_LEVEL))?),
                u64::try_from(mtime).unwrap_or(0),
            )),
            Format::Zip => Writer::Zip(zip::ZipWriter::new(
                out,
                options.level.unwrap_or(DEFAULT_LEVEL),
                mtime,
                local_time(mtime),
            )),
        };
        if let (Writer::Tar(tar), Some((id, _))) = (&mut writer, &commit) {
            tar.commit_header(*id)?;
        }
        let mut context = Context {
            objects: &objects,
            filters: &mut filters,
            lookup: &mut lookup,
            subst: subst.as_ref(),
            umask,
            big_file_threshold: big_file_threshold(self),
        };
        if options.prefix.ends_with(b"/") {
            writer.write(&options.prefix, Item::Directory, b"", tree, umask, false)?;
        }
        for entry in &entries {
            let mut path = options.prefix.clone();
            path.extend_from_slice(entry.path.get(base.len()..).unwrap_or_default());
            context.write(&mut writer, entry, path)?;
        }
        for (number, file) in (1u64..).zip(&options.extra) {
            let item = if file.executable {
                Item::Executable
            } else {
                Item::File
            };
            // Git names an added file's pax header by its position, in an otherwise zero ID.
            let mut bytes = vec![0; self.object_format().digest_len()];
            if let Some(start) = bytes.get_mut(..8) {
                start.copy_from_slice(&number.to_be_bytes());
            }
            let id = ObjectId::from_bytes(self.object_format(), &bytes).map_err(object)?;
            writer.write(&file.path, item, &file.content, id, umask, false)?;
        }
        let comment = commit
            .as_ref()
            .map(|(id, _)| id.to_string())
            .unwrap_or_default();
        writer.finish(comment.as_bytes())
    }
}

/// Reads the per-entry inputs that writing needs.
struct Context<'a> {
    objects: &'a Objects,
    filters: &'a mut Pipeline,
    lookup: &'a mut Lookup,
    subst: Option<&'a subst::Subst<'a>>,
    umask: u32,
    big_file_threshold: u64,
}

impl Context<'_> {
    fn write<W: Write>(
        &mut self,
        writer: &mut Writer<W>,
        entry: &Entry,
        path: Vec<u8>,
    ) -> Result<(), Error> {
        match entry.mode {
            EntryMode::Tree | EntryMode::Gitlink => {
                let mut path = path;
                path.push(b'/');
                writer.write(&path, Item::Directory, b"", entry.id, self.umask, false)
            }
            EntryMode::Symlink => {
                let target = self.objects.read_blob(entry.id).map_err(object)?;
                writer.write(&path, Item::Symlink, &target, entry.id, self.umask, false)
            }
            EntryMode::Blob | EntryMode::Executable => {
                let blob = self.objects.read_blob(entry.id).map_err(object)?;
                let converted = self
                    .filters
                    .to_worktree(&entry.path, &blob, entry.id)
                    .map_err(|source| Error::Filter {
                        path: entry.path.clone(),
                        source: Box::new(source),
                    })?;
                let mut content = converted.bytes.into_owned();
                if let Some(subst) = self.subst
                    && is_set(self.lookup, &entry.path, b"export-subst")?
                {
                    content = subst.expand(&entry.path, &content)?;
                }
                let item = if entry.mode == EntryMode::Executable {
                    Item::Executable
                } else {
                    Item::File
                };
                let streamed = content.len() as u64 >= self.big_file_threshold;
                writer.write(&path, item, &content, entry.id, self.umask, streamed)
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Item {
    File,
    Executable,
    Symlink,
    Directory,
}

enum Writer<W: Write> {
    Tar(tar::TarWriter<Output<W>>),
    Zip(zip::ZipWriter<W>),
}

impl<W: Write> Writer<W> {
    /// Writes one entry; for a symbolic link, `content` is its target. `streamed` marks a file at
    /// least `core.bigFileThreshold` long, which Git streams into zip archives.
    fn write(
        &mut self,
        path: &[u8],
        item: Item,
        content: &[u8],
        id: ObjectId,
        umask: u32,
        streamed: bool,
    ) -> Result<(), Error> {
        match self {
            Self::Tar(tar) => {
                let (mode, kind, size, link) = match item {
                    Item::File => (0o666 & !umask, tar::Kind::File, content.len(), &b""[..]),
                    Item::Executable => (0o777 & !umask, tar::Kind::File, content.len(), &b""[..]),
                    Item::Directory => (0o777 & !umask, tar::Kind::Directory, 0, &b""[..]),
                    Item::Symlink => (0o777, tar::Kind::Symlink, 0, content),
                };
                tar.entry(path, mode, size as u64, kind, link, id)?;
                if kind == tar::Kind::File {
                    tar.data(content)?;
                }
                Ok(())
            }
            Self::Zip(zip) => {
                let kind = match item {
                    Item::File => zip::Kind::File,
                    Item::Executable => zip::Kind::Executable,
                    Item::Symlink => zip::Kind::Symlink,
                    Item::Directory => zip::Kind::Directory,
                };
                zip.entry(path, kind, content, streamed)?
                    .map_err(|zip::TooLarge| Error::TooLarge)
            }
        }
    }

    fn finish(self, comment: &[u8]) -> Result<(), Error> {
        match self {
            Self::Tar(tar) => tar.finish()?.finish(),
            Self::Zip(zip) => zip
                .finish(comment)?
                .map(drop)
                .map_err(|zip::TooLarge| Error::TooLarge),
        }
    }
}

/// The tar byte stream's destination: the caller's writer, or a gzip stream around it.
enum Output<W: Write> {
    Plain(W),
    Gzip(Gzip<W>),
}

impl<W: Write> Output<W> {
    fn finish(self) -> Result<(), Error> {
        match self {
            Self::Plain(mut out) => Ok(out.flush()?),
            Self::Gzip(gzip) => gzip.finish(),
        }
    }
}

impl<W: Write> Write for Output<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(out) => out.write(bytes),
            Self::Gzip(gzip) => gzip.write(bytes),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(out) => out.flush(),
            Self::Gzip(gzip) => gzip.encoder.flush(),
        }
    }
}

/// A gzip member with no name and a zero time, as `git archive gzip` writes it.
struct Gzip<W: Write> {
    encoder: DeflateEncoder<W>,
    crc: crc32fast::Hasher,
    size: u32,
}

impl<W: Write> Gzip<W> {
    fn new(mut out: W, level: u32) -> io::Result<Self> {
        // The extra flags mark the slowest and fastest levels, as zlib's gzip writer does.
        let flags = match level {
            9 => 2,
            0 | 1 => 4,
            _ => 0,
        };
        out.write_all(&[0x1f, 0x8b, 8, 0, 0, 0, 0, 0, flags, 3])?;
        Ok(Self {
            encoder: DeflateEncoder::new(out, Compression::new(level)),
            crc: crc32fast::Hasher::new(),
            size: 0,
        })
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.encoder.write(bytes)?;
        let bytes = bytes.get(..written).unwrap_or_default();
        self.crc.update(bytes);
        // The gzip trailer records the size modulo 2^32.
        self.size = self.size.wrapping_add(written as u32);
        Ok(written)
    }

    fn finish(self) -> Result<(), Error> {
        let mut out = self.encoder.finish()?;
        out.write_all(&self.crc.finalize().to_le_bytes())?;
        out.write_all(&self.size.to_le_bytes())?;
        out.flush()?;
        Ok(())
    }
}

fn object(source: impl std::error::Error + Send + Sync + 'static) -> Error {
    Error::Object(Box::new(source))
}

/// The tree to archive and, when `id` names one, the commit it belongs to.
fn peel(
    objects: &Objects,
    mut id: ObjectId,
) -> Result<(ObjectId, Option<(ObjectId, Commit)>), Error> {
    loop {
        let kind = objects
            .read(id)
            .map_err(object)?
            .ok_or(Error::NotTree(id))?
            .kind();
        match kind {
            ObjectKind::Tree => return Ok((id, None)),
            ObjectKind::Commit => {
                let commit = objects.read_commit(id).map_err(object)?;
                return Ok((commit.tree(), Some((id, commit))));
            }
            ObjectKind::Tag => id = objects.read_tag(id).map_err(object)?.target(),
            ObjectKind::Blob => return Err(Error::NotTree(id)),
        }
    }
}

/// `tar.umask` as an octal number; Git's default is `002`.
fn umask(repository: &Repository) -> Result<u32, Error> {
    match repository.config().string("tar", None, "umask") {
        None => Ok(0o002),
        Some(value) => std::str::from_utf8(value)
            .ok()
            .and_then(|value| u32::from_str_radix(value, 8).ok())
            .ok_or(Error::Umask),
    }
}

/// `core.bigFileThreshold`, above which Git streams zip entries; 512 MiB by default.
fn big_file_threshold(repository: &Repository) -> u64 {
    match repository
        .config()
        .integer("core", None, "bigfilethreshold")
    {
        Ok(Some(bytes)) => u64::try_from(bytes).unwrap_or(0),
        _ => 512 * 1024 * 1024,
    }
}

/// The abbreviation length `%h` uses: `core.abbrev` when it is a number, otherwise automatic.
fn abbrev_len(repository: &Repository, objects: &Objects) -> usize {
    match repository.config().integer("core", None, "abbrev") {
        Ok(Some(length)) if length >= 4 => usize::try_from(length).unwrap_or(40),
        _ => objects.auto_abbrev_len(),
    }
}

/// `seconds` in the local time zone (`TZ`, else the system's), for zip's DOS fields.
fn local_time(seconds: i64) -> (i16, i8, i8, i8, i8, i8) {
    let Ok(timestamp) = jiff::Timestamp::from_second(seconds) else {
        return (1980, 1, 1, 0, 0, 0);
    };
    let local = timestamp.to_zoned(jiff::tz::TimeZone::system());
    (
        local.year(),
        local.month(),
        local.day(),
        local.hour(),
        local.minute(),
        local.second(),
    )
}

/// Whether `path` has `name` set, for `export-ignore` and `export-subst`.
fn is_set(lookup: &mut Lookup, path: &[u8], name: &[u8]) -> Result<bool, Error> {
    Ok(matches!(
        lookup
            .get(path, name)
            .map_err(|source| Error::Attributes(Box::new(source)))?,
        State::Set
    ))
}

/// The tree at `path` below `tree`; the empty path names `tree` itself.
fn subtree(objects: &Objects, mut tree: ObjectId, path: &[u8]) -> Result<ObjectId, Error> {
    if path.is_empty() {
        return Ok(tree);
    }
    for name in path.split(|&byte| byte == b'/') {
        let entries = objects.read_tree(tree).map_err(object)?;
        tree = entries
            .entries()
            .iter()
            .find(|entry| entry.name == name && entry.mode == EntryMode::Tree)
            .map(|entry| entry.id)
            .ok_or(Error::NotTree(tree))?;
    }
    Ok(tree)
}

/// The state of a tree walk: entry paths are `base` (the subdirectory and `/`, or empty) followed
/// by `directory`, and the pathspec applies to the part after `base`.
struct Walk<'a> {
    base: &'a [u8],
    directory: Vec<u8>,
    pathspec: &'a Pathspec,
    lookup: &'a mut Lookup,
}

/// Appends the archived entries below tree `id`, in tree order, with full paths.
fn walk(
    objects: &Objects,
    id: ObjectId,
    state: &mut Walk<'_>,
    out: &mut Vec<Entry>,
) -> Result<(), Error> {
    let tree = objects.read_tree(id).map_err(object)?;
    for entry in tree.entries() {
        let length = state.directory.len();
        if !state.directory.is_empty() {
            state.directory.push(b'/');
        }
        state.directory.extend_from_slice(&entry.name);
        let relative = state.directory.clone();
        let mut path = state.base.to_vec();
        path.extend_from_slice(&relative);
        if !is_set(state.lookup, &path, b"export-ignore")? {
            match entry.mode {
                EntryMode::Tree => {
                    let prune = state.pathspec.directory(&relative);
                    if prune != Prune::Skip {
                        let start = out.len();
                        out.push(Entry {
                            path,
                            mode: EntryMode::Tree,
                            id: entry.id,
                        });
                        walk(objects, entry.id, state, out)?;
                        // With a pathspec, a directory is archived only around a selected path.
                        if prune == Prune::Descend && out.len() == start + 1 {
                            out.truncate(start);
                        }
                    }
                }
                mode => {
                    let lookup = &mut *state.lookup;
                    let selected = state.pathspec.matches_with(&relative, |requirement| {
                        let value = lookup
                            .get(&path, requirement.name())
                            .map_err(|source| Error::Attributes(Box::new(source)))?;
                        Ok::<_, Error>(requirement.accepts(value))
                    })?;
                    if selected {
                        out.push(Entry {
                            path,
                            mode,
                            id: entry.id,
                        });
                    }
                }
            }
        }
        state.directory.truncate(length);
    }
    Ok(())
}
