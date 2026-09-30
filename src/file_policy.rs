//! Explicit modes and regular-file synchronization shared by creation and index publication.
#[cfg(unix)]
use std::fs;
use std::fs::File;
use std::io;
use std::path::Path;

/// Permission adjustment for newly created shared repository metadata.
///
/// Adjustments use the created inode's mode; the process umask is never changed. Non-Unix hosts
/// currently support only [`Self::Umask`]. Callers must qualify other platform policies separately.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SharedPermissions {
    /// Keep the operating system's creation mode and umask result.
    #[default]
    Umask,
    /// Add group read/write and directory traversal, retaining existing world permissions.
    Group,
    /// Also add world read and directory traversal; do not add world write.
    Everybody,
    /// Set exact permission bits, requiring owner read/write and a value at most `0o777`.
    ///
    /// File execute bits are discarded. Directories derive traversal from each read bit.
    /// APIs validate this value before publishing; an invalid value is never applied.
    Exact(u16),
}

impl SharedPermissions {
    pub(crate) fn validate(self) -> io::Result<()> {
        if let Self::Exact(mode) = self
            && (mode > 0o777 || mode & 0o600 != 0o600)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "exact shared mode requires owner read/write and at most 0777",
            ));
        }
        #[cfg(not(unix))]
        if self != Self::Umask {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "shared permission modes are not implemented on this platform",
            ));
        }
        Ok(())
    }

    pub(crate) fn apply_file(self, file: &File) -> io::Result<()> {
        self.validate()?;
        #[cfg(unix)]
        if self != Self::Umask {
            use std::os::unix::fs::PermissionsExt;
            let metadata = file.metadata()?;
            file.set_permissions(fs::Permissions::from_mode(
                self.mode(metadata.permissions().mode(), metadata.is_dir()),
            ))?;
        }
        #[cfg(not(unix))]
        let _ = file;
        Ok(())
    }

    pub(crate) fn apply_directory(self, path: &Path) -> io::Result<()> {
        self.validate()?;
        #[cfg(unix)]
        if self != Self::Umask {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(path)?;
            fs::set_permissions(
                path,
                fs::Permissions::from_mode(self.mode(metadata.permissions().mode(), true)),
            )?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    #[cfg(unix)]
    fn mode(self, current: u32, directory: bool) -> u32 {
        match self {
            Self::Umask => current,
            Self::Group => current | if directory { 0o070 } else { 0o060 },
            Self::Everybody => current | if directory { 0o075 } else { 0o064 },
            Self::Exact(mode) => {
                let files = u32::from(mode) & 0o666;
                (current & !0o777) | files | if directory { (files & 0o444) >> 2 } else { 0 }
            }
        }
    }
}

/// Flush regular-file contents to the platform's strongest selected file-sync primitive.
/// Directory entries are not synchronized, and no power-loss guarantee is inferred from success.
pub(crate) fn sync_file(file: &File) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        rustix::fs::fcntl_fullfsync(file).map_err(Into::into)
    }
    #[cfg(not(target_os = "macos"))]
    {
        file.sync_all()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::umask_022(SharedPermissions::Umask, 0o644, 0o644, 0o755, 0o755)]
    #[case::group_022(SharedPermissions::Group, 0o644, 0o664, 0o755, 0o775)]
    #[case::group_077(SharedPermissions::Group, 0o600, 0o660, 0o700, 0o770)]
    #[case::group_world_write(SharedPermissions::Group, 0o666, 0o666, 0o777, 0o777)]
    #[case::everybody_077(SharedPermissions::Everybody, 0o600, 0o664, 0o700, 0o775)]
    #[case::everybody_world_write(SharedPermissions::Everybody, 0o666, 0o666, 0o777, 0o777)]
    #[case::exact_660(SharedPermissions::Exact(0o660), 0o644, 0o660, 0o755, 0o770)]
    #[case::exact_664(SharedPermissions::Exact(0o664), 0o600, 0o664, 0o700, 0o775)]
    #[case::exact_no_executable_file(SharedPermissions::Exact(0o777), 0o600, 0o666, 0o700, 0o777)]
    fn observed_mode_projection(
        #[case] policy: SharedPermissions,
        #[case] file: u32,
        #[case] expected_file: u32,
        #[case] directory: u32,
        #[case] expected_directory: u32,
    ) {
        policy.validate().unwrap();
        assert_eq!(policy.mode(file, false), expected_file);
        assert_eq!(policy.mode(directory, true), expected_directory);
    }
}
