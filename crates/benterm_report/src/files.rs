//! Reading and writing the small files of the check without hanging a
//! hook or following a link somebody planted.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// Opens for reading without waiting on a pipe, and gives the file only
/// if it is a regular one.
pub(crate) fn open_regular(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is not a regular file", path.display()),
        ));
    }
    Ok(file)
}

/// Reads a regular file of at most `limit` bytes.
pub(crate) fn read_regular(path: &Path, limit: u64) -> io::Result<String> {
    let mut text = String::new();
    open_regular(path)?
        .take(limit + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} is too large", path.display()),
        ));
    }
    Ok(text)
}

/// Writes through a new file beside `path` and renames it over `path`,
/// in a folder only the user reads. A rename replaces a link instead of
/// writing through it.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let invalid = |what: &str| io::Error::new(io::ErrorKind::InvalidInput, what.to_owned());
    let parent = path.parent().ok_or_else(|| invalid("no parent"))?;
    fs::create_dir_all(parent)?;
    if !fs::symlink_metadata(parent)?.is_dir() {
        return Err(invalid("the folder of the counts is not a folder"));
    }
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| invalid("no file name"))?;
    let temporary = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    // Left behind by a process of the same id that was killed half way.
    let _ = fs::remove_file(&temporary);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&temporary)?;
    file.write_all(bytes)
        .and_then(|()| fs::rename(&temporary, path))
        .inspect_err(|_| {
            let _ = fs::remove_file(&temporary);
        })
}

/// Turns control characters into spaces, drops the characters that turn
/// the direction of text around, and shortens: text taken from a report
/// must not forge lines or hide what follows where it is shown.
pub(crate) fn clip(text: &str, max_chars: usize) -> String {
    let clean: String = text
        .chars()
        .filter(|c| !matches!(*c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if clean.chars().count() <= max_chars {
        clean
    } else {
        format!("{}…", clean.chars().take(max_chars).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::clip;

    #[test]
    fn clip_keeps_words_apart_and_drops_what_could_forge_a_line() {
        assert_eq!(clip("one\ntwo", 50), "one two");
        assert_eq!(clip("a\u{1b}b\u{202E}c", 50), "a bc");
        assert_eq!(clip("abcdef", 3), "abc…");
    }
}
