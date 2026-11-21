//! Shared path and text handling for the file-format sources.

use std::path::Path;

use crate::SourceError;

/// Reads one file as UTF-8 text.
///
/// A read failure is [`SourceError::unavailable`]. A file that is not UTF-8 is
/// [`SourceError::invalid`]: the format sources never transcode, so a document is either valid
/// UTF-8 or rejected. The failure carries no path, no bytes, and no parser text.
pub(crate) fn read_text(path: &Path) -> Result<String, SourceError> {
    let bytes = std::fs::read(path).map_err(|_| SourceError::unavailable())?;
    String::from_utf8(bytes).map_err(|_| SourceError::invalid())
}
