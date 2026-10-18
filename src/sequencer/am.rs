use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::{Action, Error, Kind, Recovery, State};
use crate::{ObjectId, mailinfo};

impl State {
    /// Prepares one queued am message after continuing or skipping the current message.
    ///
    /// Extracts the next numbered message from this snapshot with explicit bounds. Only default
    /// am options are admitted. The repository supplies its already-loaded config snapshot;
    /// configured scissors, quoted-CR or commit encoding is refused in this increment. All numbered
    /// mail is retained, the next patch remains unapplied, and the abort-safety checkpoint
    /// advances to the proposed destination. After caller-owned checkout/object/HEAD
    /// publication, publish the returned metadata and stage the next patch
    /// through [`crate::am::apply_current`] or Git. Git `am --continue` can then resume the series.
    /// Final-mail completion and session abort use [`Self::prepare`].
    ///
    /// Requires exclusive operation ownership across observation and publication. Extraction and
    /// proposed metadata admission complete before returning. Publication replaces `next` last,
    /// but is not a cross-file transaction; re-read state after any partial publication failure.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for non-am/final-mail state, unsupported options/actions, missing or
    /// malformed mail, stale safety checkpoints, exceeded bounds or cancellation. Nonempty author,
    /// email, date and subject fields are required; no current-time or missing-header fallback
    /// runs.
    pub fn prepare_next_am(
        &self,
        repository: &crate::Repository,
        action: Action,
        head: ObjectId,
        limits: mailinfo::Limits,
        cancel: &AtomicBool,
    ) -> Result<Recovery, Error> {
        if repository.git_dir() != self.git_dir || repository.object_format() != self.format {
            return Err(Error::Changed);
        }
        for (section, key) in [
            ("mailinfo", "scissors"),
            ("mailinfo", "quotedCr"),
            ("i18n", "commitEncoding"),
        ] {
            if repository.config().string(section, None, key).is_some() {
                return Err(Error::Unsupported(PathBuf::from(format!(
                    "config/{section}.{key}"
                ))));
            }
        }
        if self.kind != Kind::Am || !matches!(action, Action::Continue(_) | Action::Skip) {
            return Err(Error::Unsupported(PathBuf::from("rebase-apply")));
        }
        let (next, last) = super::state::am_range(&self.files)?;
        if next >= last {
            return Err(Error::Unsupported(PathBuf::from("rebase-apply/next")));
        }
        self.default_am_options()?;
        let mut plan = self.prepare_inner(action, head, true)?;
        let name = PathBuf::from(format!("rebase-apply/{:04}", next + 1));
        let raw = self
            .files
            .get(&name)
            .ok_or_else(|| Error::Unsupported(name.clone()))?;
        let mail = mailinfo::parse_with(raw, mailinfo::Options::default(), limits, cancel)?;
        let required = |value: Option<String>| {
            value
                .filter(|value| !value.is_empty())
                .ok_or_else(|| Error::Unsupported(name.clone()))
        };
        let author = required(mail.author)?;
        let email = required(mail.email)?;
        let date = required(mail.date)?;
        let subject = required(mail.subject)?;
        if [&author, &email, &date, &subject]
            .iter()
            .any(|value| value.bytes().any(|byte| matches!(byte, 0 | b'\r' | b'\n')))
            || mail.patch.is_empty()
        {
            return Err(Error::Unsupported(name));
        }
        let mut script = Vec::new();
        for (key, value) in [
            ("GIT_AUTHOR_NAME", &author),
            ("GIT_AUTHOR_EMAIL", &email),
            ("GIT_AUTHOR_DATE", &date),
        ] {
            script.extend_from_slice(key.as_bytes());
            script.extend_from_slice(b"='");
            for byte in value.bytes() {
                if matches!(byte, b'\'' | b'!') {
                    script.extend_from_slice(b"'\\");
                    script.push(byte);
                    script.push(b'\'');
                } else {
                    script.push(byte);
                }
            }
            script.extend_from_slice(b"'\n");
        }
        let mut message = subject.as_bytes().to_vec();
        message.push(b'\n');
        if !mail.message.is_empty() {
            message.push(b'\n');
            message.extend_from_slice(&mail.message);
        }
        let info =
            format!("Author: {author}\nEmail: {email}\nSubject: {subject}\nDate: {date}\n\n");
        for (path, bytes) in &self.files {
            if path.starts_with("rebase-apply")
                || matches!(action, Action::Continue(_))
                    && matches!(path.to_str(), Some("MERGE_MSG" | "AUTO_MERGE"))
            {
                plan.after.insert(path.clone(), bytes.clone());
            }
        }
        for (name, bytes) in [
            ("next", format!("{}\n", next + 1).into_bytes()),
            (
                "abort-safety",
                format!("{}\n", plan.destination).into_bytes(),
            ),
            ("author-script", script),
            ("final-commit", message),
            ("info", info.into_bytes()),
            ("msg", mail.message),
            ("patch", mail.patch),
        ] {
            plan.after
                .insert(Path::new("rebase-apply").join(name), bytes);
        }
        let total = plan
            .after
            .values()
            .try_fold(0usize, |sum, bytes| sum.checked_add(bytes.len()))
            .ok_or(Error::Limit)?;
        if plan.after.len() > self.limits.max_files || total > self.limits.max_bytes {
            return Err(Error::Limit);
        }
        Ok(plan)
    }

    fn default_am_options(&self) -> Result<(), Error> {
        for (name, expected) in [
            ("apply-opt", b"".as_slice()),
            ("keep", b"f\n"),
            ("messageid", b"f\n"),
            ("quiet", b"f\n"),
            ("quoted-cr", b""),
            ("scissors", b""),
            ("sign", b"f\n"),
            ("threeway", b"f\n"),
            ("utf8", b"t\n"),
        ] {
            let path = Path::new("rebase-apply").join(name);
            if self.files.get(&path).map(Vec::as_slice) != Some(expected) {
                return Err(Error::Unsupported(path));
            }
        }
        Ok(())
    }
}
