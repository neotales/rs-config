//! Shared path and text handling for the file-format sources.

use std::{io, path::Path};

use crate::SourceError;

/// Reads one file as bytes.
///
/// `optional` controls exactly one case: an actual [`io::ErrorKind::NotFound`] from opening the
/// path becomes `Ok(None)`. Every other failure is [`SourceError::unavailable`]: a path that is a
/// directory, a permission error, a path with a non-directory component, an I/O error during the
/// read, and a path that disappeared after it was opened. The decision is taken from the open and
/// read result itself, never from a preceding existence check, so there is no window in which a
/// file can appear or vanish between the check and the read.
///
/// The reload fingerprint draws the same line, so an optional path that exists but cannot be read
/// is never mistaken for an absent one: the poll attempts the load and reports the failure instead
/// of assuming the file is unchanged.
///
/// [`SourceError::unavailable`] carries no path, no bytes, and no operating-system message.
pub(crate) fn read_bytes(path: &Path, optional: bool) -> Result<Option<Vec<u8>>, SourceError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if optional && error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(SourceError::unavailable()),
    }
}

/// Reads one file as UTF-8 text.
///
/// A file that is not UTF-8 is [`SourceError::invalid`]: the format sources never transcode, so a
/// document is either valid UTF-8 or rejected. An absent optional file is `Ok(None)`; see
/// [`read_bytes`].
///
/// The parser needs the whole document as one `&str`, so this intentionally holds the document
/// once. It is not used for change detection: the reload fingerprint streams the same path in
/// fixed chunks and never builds a whole-file buffer.
#[cfg(any(
    feature = "json",
    feature = "json5",
    feature = "jsonc",
    feature = "toml",
    feature = "yaml",
    feature = "ini"
))]
pub(crate) fn read_text(path: &Path, optional: bool) -> Result<Option<String>, SourceError> {
    match read_bytes(path, optional)? {
        Some(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| SourceError::invalid()),
        None => Ok(None),
    }
}
