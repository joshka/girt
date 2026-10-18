use super::*;

impl Scan<'_> {
    /// Returns names and whether stale inherited ignore state invalidates descendant cache use.
    pub(super) fn cached_names(
        &self,
        fd: &OwnedFd,
        before: &Stat,
        path: &[u8],
    ) -> Result<(Option<Names>, bool), Error> {
        let Some(directory) = self
            .untracked_cache
            .and_then(|cache| cache.directories.get(path))
        else {
            return Ok((None, true));
        };

        let cached = &directory.cached;
        let Some(stat) = cached.stat().filter(|_| cached.valid()) else {
            return Ok((None, true));
        };

        let Some(written) = self.index_written else {
            return Ok((None, true));
        };

        if stat != index_stat(before)
            || stat.mtime.seconds >= written
            || stat.ctime.seconds >= written
        {
            return Ok((None, true));
        }

        let content = match optional_stat(fd, b".gitignore", path)? {
            Some(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {
                let content = self.read_file(
                    fd,
                    b".gitignore",
                    path,
                    &stat,
                    self.max_file_bytes.min(self.worktree_bytes.remaining()),
                )?;
                self.worktree_bytes
                    .charge(content.len(), "worktree bytes")?;
                Some(content)
            }
            _ => None,
        };
        let ignore = content
            .as_ref()
            .map(|content| crate::status::untracked::ignore_id(self.format, content));
        // Git can reuse a tracked ignore file's index blob identity. A file loaded from
        // the filesystem instead uses the newline-extended ignore buffer identity. Both
        // forms bind these exact bytes; neither permits accepting different ignore rules.
        let literal = content
            .as_ref()
            .map(|bytes| ObjectId::for_blob(self.format, bytes));
        let expected = cached
            .exclude()
            .filter(|id| *id != ObjectId::null(self.format));
        if expected != ignore && expected != literal {
            return Ok((None, true));
        }

        // Git can omit metadata aliases from UNTR. If any alias resolves, obtain actual names
        // through readdir rather than guessing the spelling on a case-insensitive filesystem.
        for variant in 1u8..8 {
            let mut alias = *b".git";
            for bit in 0..3 {
                if variant & (1 << bit) != 0 {
                    alias[bit + 1].make_ascii_uppercase();
                }
            }
            if optional_stat(fd, &alias, path)?.is_some() {
                let mut names = self.read_names(fd, path)?;
                names.ignore_observed = true;
                names.ignore_content = content;
                return Ok((Some(names), false));
            }
        }

        let mut children: Vec<Vec<u8>> = self
            .tracked
            .children(path)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        children.extend(directory.children.iter().cloned());
        children.extend(
            cached
                .untracked()
                .iter()
                .map(|name| name.strip_suffix(b"/").unwrap_or(name).to_vec()),
        );

        if ignore.is_some() {
            children.push(b".gitignore".to_vec());
        }

        children.sort_unstable();
        children.dedup();

        let mut names = Names {
            cached: true,
            ignore_observed: true,
            ignore_content: content,
            ..Names::default()
        };

        for name in children {
            self.check()?;
            validate_path(&join(path, &name))?;

            self.directory_entries.charge(1, "directory entries")?;
            let length = path
                .len()
                .checked_add(name.len())
                .and_then(|n| n.checked_add(usize::from(!path.is_empty())))
                .ok_or(Error::Limit("path bytes"))?;
            self.path_bytes.charge(length, "path bytes")?;

            names.bytes.extend_from_slice(&name);
            names.ends.push(names.bytes.len());
        }

        Ok((Some(names), false))
    }
}
