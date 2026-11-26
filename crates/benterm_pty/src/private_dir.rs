//! The directory of the files every terminal runs or sources: the
//! helper and the shell integration.
//!
//! It is in the user's state directory, beside the app's socket, and not
//! in the temporary directory: a coding agent's sandbox lets the agent
//! write in the temporary directory, and what is written there would run
//! in the next terminal, outside the sandbox.

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// `$XDG_STATE_HOME/benterm/pty`, or `~/.local/state/benterm/pty` when
/// `XDG_STATE_HOME` is not set to an absolute path: made if it is
/// missing, and checked to be the user's own (see [`make_private`]).
pub(crate) fn private_dir() -> io::Result<PathBuf> {
    let dir = dir_in(
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME").or_else(|| crate::sys::passwd_home().map(PathBuf::into_os_string)),
    )
    .ok_or_else(|| io::Error::other("no home directory to keep the terminal's files in"))?;
    make_private(&dir)?;
    Ok(dir)
}

fn dir_in(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let state = xdg_state_home
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            home.map(PathBuf::from)
                .filter(|home| home.is_absolute())
                .map(|home| home.join(".local/state"))
        })?;
    Some(state.join("benterm").join("pty"))
}

/// Makes `dir` for the user alone if it is missing, and fails unless it
/// is a directory, not a link to one, that belongs to the user and that
/// nobody else may write in: its files are run and sourced.
///
/// Nor may anybody else write in a folder above it, where they could
/// put another directory in its place: each belongs to the user or to
/// root, and is one that only they may write in, or one with the sticky
/// bit, as `/tmp` has, in which others may take out only what is theirs.
pub(crate) fn make_private(dir: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("not the user's own directory: {}", dir.display()),
        ));
    }
    for parent in dir
        .ancestors()
        .skip(1)
        .filter(|p| !p.as_os_str().is_empty())
    {
        let meta = fs::metadata(parent)?;
        let owned = meta.uid() == uid || meta.uid() == 0;
        let sticky = meta.mode() & 0o1000 != 0;
        if !owned || (meta.mode() & 0o022 != 0 && !sticky) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} is in {}, which others may write in",
                    dir.display(),
                    parent.display()
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_is_in_the_state_directory_and_never_a_relative_one() {
        let os = |text: &str| Some(OsString::from(text));
        assert_eq!(
            dir_in(os("/state"), os("/home/u")),
            Some(PathBuf::from("/state/benterm/pty"))
        );
        assert_eq!(
            dir_in(os("relative"), os("/home/u")),
            Some(PathBuf::from("/home/u/.local/state/benterm/pty"))
        );
        assert_eq!(
            dir_in(None, os("/home/u")),
            Some(PathBuf::from("/home/u/.local/state/benterm/pty"))
        );
        assert_eq!(dir_in(None, os("relative")), None);
        assert_eq!(dir_in(None, None), None);
    }

    #[test]
    fn a_link_or_a_directory_others_may_write_in_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(make_private(&open).is_err());

        let real = root.path().join("real");
        make_private(&real).unwrap();
        assert_eq!(fs::metadata(&real).unwrap().mode() & 0o777, 0o700);
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(make_private(&link).is_err());
    }

    /// Whoever may write in a folder above the directory may put another
    /// directory in its place, with files of theirs to run.
    #[test]
    fn a_directory_under_a_folder_others_may_write_in_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let open = root.path().join("open");
        fs::create_dir(&open).unwrap();
        fs::set_permissions(&open, fs::Permissions::from_mode(0o777)).unwrap();
        let dir = open.join("state/benterm/pty");
        assert!(make_private(&dir).is_err());

        // Unless others may only take out what is theirs, as in /tmp.
        fs::set_permissions(&open, fs::Permissions::from_mode(0o1777)).unwrap();
        make_private(&dir).unwrap();

        let group = root.path().join("group");
        fs::create_dir(&group).unwrap();
        fs::set_permissions(&group, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(make_private(&group.join("pty")).is_err());
    }
}
