//! What the file browser shows of a file: the start of its text, or why
//! it shows none. Files are opened for reading only, and no more than the
//! start of a file is read.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::markdown::{Block, parse};

/// How many bytes of a file are read at most.
pub const PREVIEW_LIMIT: usize = 64 * 1024;

/// What is shown of a file.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    /// The text of a Markdown file, formatted.
    Markdown(Vec<Block>),
    /// A file that is not text, of which nothing is shown.
    NotText,
    /// Not a regular file, such as a folder, a device or a named pipe,
    /// which is not opened.
    NotRegularFile,
    /// Why the file could not be read.
    Unreadable(String),
}

/// A file as the browser shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Preview {
    pub content: Content,
    /// The size of the whole file in bytes.
    pub size: u64,
    /// Only the start of the file is shown.
    pub truncated: bool,
}

/// Reads the first `limit` bytes of `path` and tells what to show of them.
///
/// Only a regular file is opened, as the symbolic links lead. Opening a
/// named pipe waits until something writes to it, and opening a device
/// can wait or do something, so neither is opened.
pub fn read_preview(path: &Path, limit: usize) -> Preview {
    match std::fs::metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Preview {
                content: Content::NotRegularFile,
                size: 0,
                truncated: false,
            };
        }
        Ok(_) => {}
        Err(error) => {
            return Preview {
                content: Content::Unreadable(error.to_string()),
                size: 0,
                truncated: false,
            };
        }
    }
    match read_start(path, limit) {
        Ok((bytes, size, truncated)) => Preview {
            content: match text_of(bytes, truncated) {
                Content::Text(text) if is_markdown(path) => Content::Markdown(parse(&text)),
                content => content,
            },
            size,
            truncated,
        },
        Err(error) => Preview {
            content: Content::Unreadable(error.to_string()),
            size: 0,
            truncated: false,
        },
    }
}

/// The first `limit` bytes of `path`, the size of the file, and whether
/// there is more. One byte more is read to tell, in case the file grew.
fn read_start(path: &Path, limit: usize) -> std::io::Result<(Vec<u8>, u64, bool)> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > limit;
    bytes.truncate(limit);
    Ok((bytes, size, truncated))
}

/// The text of `bytes`, or `NotText` if they are not UTF-8 or hold a NUL.
/// A character cut by the end of a truncated start is left out.
fn text_of(mut bytes: Vec<u8>, truncated: bool) -> Content {
    if bytes.contains(&0) {
        return Content::NotText;
    }
    match std::str::from_utf8(&bytes) {
        Ok(_) => {}
        Err(error) if truncated && error.error_len().is_none() => {
            bytes.truncate(error.valid_up_to());
        }
        Err(_) => return Content::NotText,
    }
    String::from_utf8(bytes).map_or(Content::NotText, Content::Text)
}

/// Whether `path` is named as a Markdown file.
fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["md", "markdown"]
                .iter()
                .any(|md| extension.eq_ignore_ascii_case(md))
        })
}
