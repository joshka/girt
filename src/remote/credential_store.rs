//! Native `credential.helper = store`: plain-text credential files in Git's documented format.
//!
//! Each line of a credential file is a URL `protocol://user:password@host/path` whose username,
//! password, host and path are percent-encoded. Lines that don't parse are skipped when reading and
//! kept verbatim when rewriting. The behavior follows gitcredentials(7), git-credential-store(1)
//! and observations of `git credential-store` (Git 2.56).

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use super::credential::{Credential, CredentialContext, CredentialError};

/// The credential files used by Git's `store` helper.
///
/// Lookups read the files in order and return the first matching entry. Approval writes to the
/// first file that exists, or creates the first file when none does. Rejection removes matching
/// entries from every file. Each change holds a `<file>.lock` sibling from before the read until
/// it is renamed over the file, so concurrent Git or girt writers fail instead of losing updates
/// and an interrupted write leaves the old contents. New files are mode 0600; a rewrite drops group
/// and other access, as Git does, and keeps stricter owner bits such as 0400.
///
/// ```
/// use std::path::Path;
///
/// use girt::remote::CredentialStore;
///
/// let store = CredentialStore::user_files(Some(Path::new("/home/user")), None);
/// assert_eq!(
///     store.files(),
///     [
///         Path::new("/home/user/.git-credentials"),
///         Path::new("/home/user/.config/git/credentials"),
///     ]
/// );
/// ```
#[derive(Clone)]
pub struct CredentialStore {
    files: Vec<PathBuf>,
}

impl std::fmt::Debug for CredentialStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialStore")
            .field("files", &self.files)
            .finish()
    }
}

impl CredentialStore {
    /// Uses only `path`, like `git credential-store --file <path>`.
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            files: vec![path.into()],
        }
    }

    /// Uses Git's default files: `~/.git-credentials`, then `$XDG_CONFIG_HOME/git/credentials`.
    ///
    /// When `xdg_config_home` is `None` or empty, the second file is `~/.config/git/credentials`.
    /// Without a home directory, only an explicit XDG file remains.
    pub fn user_files(home: Option<&Path>, xdg_config_home: Option<&Path>) -> Self {
        let xdg = xdg_config_home
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .or_else(|| home.map(|home| home.join(".config")));
        let files = home
            .map(|home| home.join(".git-credentials"))
            .into_iter()
            .chain(xdg.map(|xdg| xdg.join("git").join("credentials")))
            .collect();
        Self { files }
    }

    /// Files in lookup order.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Fills `credential` from the first matching entry. Missing files are skipped.
    pub(crate) fn get(
        &self,
        context: &CredentialContext,
        credential: &mut Credential,
    ) -> Result<(), CredentialError> {
        let query = Query::new(context, credential, false);
        for file in &self.files {
            let Some(contents) = read(file)? else {
                continue;
            };
            if let Some(entry) = lines(&contents)
                .filter_map(Entry::parse)
                .find(|entry| query.matches(entry))
            {
                credential.username = Some(entry.username);
                credential.password = Some(entry.password);
                return Ok(());
            }
        }
        Ok(())
    }

    /// Saves a complete credential, replacing entries for the same user and location.
    pub(crate) fn store(
        &self,
        context: &CredentialContext,
        credential: &Credential,
    ) -> Result<(), CredentialError> {
        let (Some(username), Some(password)) = (&credential.username, &credential.password) else {
            return Ok(());
        };
        // Like Git: the first file that exists, else the first file.
        let Some(file) = self
            .files
            .iter()
            .find(|file| file.exists())
            .or_else(|| self.files.first())
        else {
            return Ok(());
        };
        let lock = Lock::acquire(file)?;
        let contents = lock.read()?.unwrap_or_default();
        let query = Query::new(context, credential, false);
        let mut output = Entry {
            protocol: context.protocol.clone(),
            username: username.clone(),
            password: password.clone(),
            host: context.host.clone(),
            path: query.path.map(<[u8]>::to_vec),
        }
        .format();
        output.push(b'\n');
        for line in lines(&contents) {
            if !Entry::parse(line).is_some_and(|entry| query.matches(&entry)) {
                output.extend_from_slice(line);
                output.push(b'\n');
            }
        }
        lock.commit(&output)
    }

    /// Removes matching entries from every file. A supplied password must also match.
    pub(crate) fn erase(
        &self,
        context: &CredentialContext,
        credential: &Credential,
    ) -> Result<(), CredentialError> {
        let query = Query::new(context, credential, true);
        for file in self.files.iter().filter(|file| file.exists()) {
            let lock = Lock::acquire(file)?;
            let Some(contents) = lock.read()? else {
                continue;
            };
            let mut output = Vec::with_capacity(contents.len());
            let mut erased = false;
            for line in lines(&contents) {
                if Entry::parse(line).is_some_and(|entry| query.matches(&entry)) {
                    erased = true;
                } else {
                    output.extend_from_slice(line);
                    output.push(b'\n');
                }
            }
            if erased {
                lock.commit(&output)?;
            }
        }
        Ok(())
    }
}

/// Fields a stored entry must equal. Absent fields match any entry.
struct Query<'a> {
    protocol: &'a [u8],
    host: &'a [u8],
    path: Option<&'a [u8]>,
    username: Option<&'a [u8]>,
    password: Option<&'a [u8]>,
}

impl<'a> Query<'a> {
    fn new(context: &'a CredentialContext, credential: &'a Credential, password: bool) -> Self {
        Self {
            protocol: &context.protocol,
            host: &context.host,
            path: context.path.as_deref().filter(|_| context.use_http_path),
            username: credential.username.as_deref(),
            password: credential.password.as_deref().filter(|_| password),
        }
    }

    fn matches(&self, entry: &Entry) -> bool {
        self.protocol == entry.protocol
            && self.host == entry.host
            && self
                .path
                .is_none_or(|path| entry.path.as_deref() == Some(path))
            && self.username.is_none_or(|name| name == entry.username)
            && self.password.is_none_or(|secret| secret == entry.password)
    }
}

/// One decoded line of a credential file.
struct Entry {
    protocol: Vec<u8>,
    username: Vec<u8>,
    password: Vec<u8>,
    host: Vec<u8>,
    path: Option<Vec<u8>>,
}

impl Entry {
    /// Decodes `protocol://user:password@host[/path]`. Lines without a username and password, or
    /// with a field that decodes to a line feed, are not entries. Trailing slashes are not part of
    /// the path.
    fn parse(line: &[u8]) -> Option<Self> {
        let separator = line.windows(3).position(|part| part == b"://")?;
        let protocol = &line[..separator];
        let rest = &line[separator + 3..];
        let authority_end = rest.iter().position(|b| *b == b'/').unwrap_or(rest.len());
        let (authority, path) = rest.split_at(authority_end);
        let at = authority.iter().rposition(|b| *b == b'@')?;
        let (user_info, host) = (&authority[..at], &authority[at + 1..]);
        let colon = user_info.iter().position(|b| *b == b':')?;
        let path = path.strip_prefix(b"/").unwrap_or(path);
        let path_end = path
            .iter()
            .rposition(|b| *b != b'/')
            .map_or(0, |end| end + 1);
        let path = &path[..path_end];
        let entry = Self {
            protocol: protocol.to_vec(),
            username: decode(&user_info[..colon]),
            password: decode(&user_info[colon + 1..]),
            host: decode(host),
            path: (!path.is_empty()).then(|| decode(path)),
        };
        // Git skips (and keeps) lines whose fields decode to a line feed.
        let fields = [&entry.username, &entry.password, &entry.host];
        let has_newline = fields
            .into_iter()
            .chain(&entry.path)
            .any(|field| field.contains(&b'\n'));
        (!has_newline).then_some(entry)
    }

    fn format(&self) -> Vec<u8> {
        let mut line = self.protocol.clone();
        line.extend_from_slice(b"://");
        encode(&mut line, &self.username, false);
        line.push(b':');
        encode(&mut line, &self.password, false);
        line.push(b'@');
        encode(&mut line, &self.host, false);
        if let Some(path) = &self.path {
            line.push(b'/');
            encode(&mut line, path, true);
        }
        line
    }
}

/// Percent-encodes everything except RFC 3986 unreserved bytes (and `/` in paths), using
/// lowercase hex as Git does.
fn encode(output: &mut Vec<u8>, value: &[u8], path: bool) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for &byte in value {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) || (path && byte == b'/') {
            output.push(byte);
        } else {
            output.extend_from_slice(&[
                b'%',
                HEX[usize::from(byte >> 4)],
                HEX[usize::from(byte & 15)],
            ]);
        }
    }
}

/// Decodes `%XX` escapes. `%00` and a `%` without two hex digits stay literal, as in Git.
fn decode(value: &[u8]) -> Vec<u8> {
    let hex = |byte: u8| (byte as char).to_digit(16).map(|digit| digit as u8);
    let mut output = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] == b'%'
            && let Some(high) = value.get(index + 1).copied().and_then(hex)
            && let Some(low) = value.get(index + 2).copied().and_then(hex)
            && (high, low) != (0, 0)
        {
            output.push(high << 4 | low);
            index += 3;
        } else {
            output.push(value[index]);
            index += 1;
        }
    }
    output
}

/// Lines without their `\n` terminators. A final unterminated line is included.
fn lines(contents: &[u8]) -> impl Iterator<Item = &[u8]> {
    contents
        .strip_suffix(b"\n")
        .unwrap_or(contents)
        .split(|b| *b == b'\n')
        .filter(move |_| !contents.is_empty())
}

/// A held `<file>.lock` for one read-modify-write of a credential file (or its symlink target).
///
/// The lock is created exclusively before the file is read and is renamed over the file to publish
/// the new contents, so a concurrent Git or girt writer fails rather than losing an update. An
/// interrupted write leaves the old contents. Dropping an uncommitted lock removes it.
struct Lock {
    file: PathBuf,
    lock: PathBuf,
    handle: Option<std::fs::File>,
    cleanup: crate::locks::Guard,
}

impl Lock {
    fn acquire(file: &Path) -> Result<Self, CredentialError> {
        let _lock_operation = crate::locks::operation();
        let file = match std::fs::canonicalize(file) {
            Ok(target) => target,
            Err(error) if error.kind() == ErrorKind::NotFound => file.to_path_buf(),
            Err(source) => return Err(write_error(source)),
        };
        let mut lock = file.as_os_str().to_owned();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        // New files are owner-only. A rewrite keeps stricter existing bits (such as 0400) and,
        // like Git, drops group and other access.
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            let mode = match std::fs::metadata(&file) {
                Ok(metadata) => metadata.permissions().mode() & 0o600,
                Err(_) => 0o600,
            };
            options.mode(mode);
        }
        let handle = options.open(&lock).map_err(write_error)?;
        let cleanup = crate::locks::Guard::new(&lock, &handle).map_err(write_error)?;
        Ok(Self {
            file,
            lock,
            handle: Some(handle),
            cleanup,
        })
    }

    fn read(&self) -> Result<Option<Vec<u8>>, CredentialError> {
        read(&self.file)
    }

    fn commit(mut self, contents: &[u8]) -> Result<(), CredentialError> {
        let _lock_operation = crate::locks::operation();
        self.cleanup.check_path().map_err(write_error)?;
        let mut handle = self.handle.take().expect("uncommitted lock");
        let published = handle
            .write_all(contents)
            .and_then(|()| handle.sync_all())
            .and_then(|()| {
                drop(handle);
                std::fs::rename(&self.lock, &self.file)
            });
        if published.is_err() && self.cleanup.check_path().is_ok() {
            let _ = std::fs::remove_file(&self.lock);
        }
        published.map_err(write_error)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _lock_operation = crate::locks::operation();
        if self.handle.take().is_some() && self.cleanup.check_path().is_ok() {
            let _ = std::fs::remove_file(&self.lock);
        }
    }
}

fn write_error(source: std::io::Error) -> CredentialError {
    CredentialError::Io {
        stage: "store write",
        source,
    }
}

fn read(file: &Path) -> Result<Option<Vec<u8>>, CredentialError> {
    match std::fs::read(file) {
        Ok(contents) => Ok(Some(contents)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(source) => Err(CredentialError::Io {
            stage: "store read",
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(path: Option<&str>) -> CredentialContext {
        CredentialContext {
            protocol: b"https".to_vec(),
            host: b"example.invalid:8443".to_vec(),
            path: path.map(|path| path.as_bytes().to_vec()),
            use_http_path: path.is_some(),
        }
    }

    fn credential(username: Option<&str>, password: Option<&str>) -> Credential {
        Credential {
            username: username.map(|value| value.as_bytes().to_vec()),
            password: password.map(|value| value.as_bytes().to_vec()),
        }
    }

    fn lookup(store: &CredentialStore, context: &CredentialContext) -> Option<(String, String)> {
        let mut found = Credential::default();
        store.get(context, &mut found).unwrap();
        Some((
            String::from_utf8(found.username?).unwrap(),
            String::from_utf8(found.password?).unwrap(),
        ))
    }

    #[test]
    fn encodes_reserved_bytes_like_git() {
        let entry = Entry {
            protocol: b"https".to_vec(),
            username: b"a-_.~!*()';:@&=+$,/?#[] \tz".to_vec(),
            password: b"p:w/d%".to_vec(),
            host: b"example.invalid:8443".to_vec(),
            path: Some(b"x y/%z".to_vec()),
        };
        // Observed from `git credential-store store` for the same fields.
        let line = b"https://a-_.~%21%2a%28%29%27%3b%3a%40%26%3d%2b%24%2c%2f%3f%23%5b%5d%20%09z\
            :p%3aw%2fd%25@example.invalid%3a8443/x%20y/%25z";
        assert_eq!(entry.format(), line);
        let parsed = Entry::parse(line).unwrap();
        assert_eq!(parsed.username, entry.username);
        assert_eq!(parsed.password, entry.password);
        assert_eq!(parsed.host, entry.host);
        assert_eq!(parsed.path, entry.path);
    }

    #[test]
    fn skips_lines_without_credentials_and_keeps_invalid_escapes() {
        for line in [&b"garbage"[..], b"https://host", b"https://user@host", b""] {
            assert!(Entry::parse(line).is_none());
        }
        let entry = Entry::parse(b"https://u%zz:p%4@host/a//").unwrap();
        assert_eq!(entry.username, b"u%zz");
        assert_eq!(entry.password, b"p%4");
        assert_eq!(entry.path.as_deref(), Some(&b"a"[..]));
        assert!(Entry::parse(b"https://u:p@host/").unwrap().path.is_none());
    }

    #[test]
    fn lookup_matches_exact_fields_and_treats_missing_query_fields_as_wildcards() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        std::fs::write(
            &file,
            "https://other:1@example.invalid%3a8443\n\
             https://user:2@example.invalid%3a8443/repo\n\
             https://user:3@EXAMPLE.invalid%3a8443\n",
        )
        .unwrap();
        let store = CredentialStore::file(&file);
        assert_eq!(
            lookup(&store, &context(None)),
            Some(("other".into(), "1".into()))
        );
        assert_eq!(
            lookup(&store, &context(Some("repo"))),
            Some(("user".into(), "2".into()))
        );
        assert_eq!(lookup(&store, &context(Some("other"))), None);
        let mut named = credential(Some("user"), None);
        store.get(&context(None), &mut named).unwrap();
        assert_eq!(named.password.as_deref(), Some(&b"2"[..]));
    }

    #[test]
    fn store_replaces_matching_entries_and_keeps_damaged_lines() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        std::fs::write(
            &file,
            "damaged\nhttps://user:old@example.invalid%3a8443/repo\nhttps://kept:1@example.invalid%3a8443\n",
        )
        .unwrap();
        let store = CredentialStore::file(&file);
        store
            .store(&context(None), &credential(Some("user"), Some("new")))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "https://user:new@example.invalid%3a8443\ndamaged\nhttps://kept:1@example.invalid%3a8443\n"
        );
        store
            .store(&context(None), &credential(Some("user"), None))
            .unwrap();
        assert!(!directory.path().join("credentials.lock").exists());
    }

    #[test]
    fn erase_requires_a_supplied_password_to_match() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        let original = "https://user:secret@example.invalid%3a8443\n";
        std::fs::write(&file, original).unwrap();
        let store = CredentialStore::file(&file);
        store
            .erase(&context(None), &credential(Some("user"), Some("wrong")))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
        store
            .erase(&context(None), &credential(None, None))
            .unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
    }

    #[test]
    fn default_files_prefer_existing_home_file_then_xdg_then_create_home_file() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path();
        let xdg = home.join("xdg");
        let store = CredentialStore::user_files(Some(home), Some(&xdg));
        let complete = credential(Some("user"), Some("secret"));
        store.store(&context(None), &complete).unwrap();
        assert!(home.join(".git-credentials").exists());
        std::fs::remove_file(home.join(".git-credentials")).unwrap();
        std::fs::create_dir_all(xdg.join("git")).unwrap();
        std::fs::write(xdg.join("git/credentials"), "").unwrap();
        store.store(&context(None), &complete).unwrap();
        assert!(!home.join(".git-credentials").exists());
        assert_eq!(
            lookup(&store, &context(None)),
            Some(("user".into(), "secret".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn rewrite_is_owner_only_and_follows_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        std::fs::write(&target, "").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
        let link = directory.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        CredentialStore::file(&link)
            .store(&context(None), &credential(Some("user"), Some("secret")))
            .unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn existing_lock_refuses_write_and_keeps_file() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        std::fs::write(&file, "https://user:old@example.invalid%3a8443\n").unwrap();
        std::fs::write(directory.path().join("credentials.lock"), "").unwrap();
        let result = CredentialStore::file(&file)
            .store(&context(None), &credential(Some("user"), Some("new")));
        assert!(matches!(result, Err(CredentialError::Io { .. })));
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "https://user:old@example.invalid%3a8443\n"
        );
    }

    #[test]
    fn concurrent_writers_do_not_lose_updates() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        std::fs::write(&file, "").unwrap();
        let store = CredentialStore::file(&file);
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let store = &store;
                scope.spawn(move || {
                    for entry in 0..25 {
                        let mut context = context(None);
                        context.host = format!("host-{writer}-{entry}").into_bytes();
                        let complete = credential(Some("user"), Some("secret"));
                        // A held lock fails the write; retry like a caller would.
                        while store.store(&context, &complete).is_err() {
                            std::thread::yield_now();
                        }
                    }
                });
            }
        });
        let contents = std::fs::read_to_string(&file).unwrap();
        assert_eq!(contents.lines().count(), 8 * 25);
    }

    #[test]
    fn lines_decoding_to_line_feeds_are_kept_and_nul_escapes_stay_literal() {
        // Observed from `git credential-store` (Git 2.56): these lines are skipped by `get` and
        // survive a `store` for the same host; `%00` is returned undecoded and `%0d` decoded.
        let foreign = "https://u:foreign%0aline@example.invalid%3a8443\n\
                       https://u%0a:p@example.invalid%3a8443\n";
        assert!(lines(foreign.as_bytes()).all(|line| Entry::parse(line).is_none()));
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        std::fs::write(
            &file,
            format!("https://u:p@example.invalid%3a8443\n{foreign}"),
        )
        .unwrap();
        let store = CredentialStore::file(&file);
        store
            .store(&context(None), &credential(Some("u"), Some("n")))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            format!("https://u:n@example.invalid%3a8443\n{foreign}")
        );
        let entry = Entry::parse(b"https://u:p%00q%0dr@host").unwrap();
        assert_eq!(entry.password, b"p%00q\rr");
    }

    #[cfg(unix)]
    #[test]
    fn rewrite_keeps_stricter_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("credentials");
        std::fs::write(&file, "").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        CredentialStore::file(&file)
            .store(&context(None), &credential(Some("user"), Some("secret")))
            .unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o400);
    }
}
