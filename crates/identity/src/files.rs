//! How identity records are kept in the Capsule: a key log is one file,
//! one entry per line, each the signed envelope's bytes in hexadecimal
//! exactly as the identity library made them. A file is written whole and
//! renamed into place, so it never holds half a log.

use std::path::Path;

use id_core::Signed;

/// Why a log file could not be read or written.
#[derive(Debug)]
pub enum FileError {
    Io(std::io::Error),
    /// An entry does not decode.
    Entry,
}

/// The log in `path`, or `None` when there is no such file.
pub fn read_log(path: &Path) -> Result<Option<Vec<Signed>>, FileError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(FileError::Io(e)),
    };
    text.lines()
        .map(|line| unhex(line.trim()).and_then(|b| Signed::from_bytes(&b).ok()))
        .collect::<Option<Vec<_>>>()
        .map(Some)
        .ok_or(FileError::Entry)
}

/// Write `entries` to `path`, whole.
pub fn write_log(path: &Path, entries: &[Signed]) -> Result<(), FileError> {
    let mut text = String::new();
    for entry in entries {
        text.push_str(&hex(&entry.to_bytes().map_err(|_| FileError::Entry)?));
        text.push('\n');
    }
    write_whole(path, text.as_bytes()).map_err(FileError::Io)
}

/// Write `bytes` to `path` by way of a `.part` file, synced, then renamed.
pub fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = std::path::PathBuf::from(part);
    {
        let mut file = std::fs::File::create(&part)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&part, path)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FileError::Io(e) => write!(f, "{e}"),
            FileError::Entry => write!(f, "an entry does not decode"),
        }
    }
}
