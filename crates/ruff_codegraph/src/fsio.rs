//! The only module that touches the file system.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::Error;

pub(crate) fn read(path: &Path) -> Result<Vec<u8>, Error> {
    fs::read(path).map_err(|e| Error::io(path, e))
}

pub(crate) fn read_text(path: &Path) -> Result<String, Error> {
    let bytes = read(path)?;
    String::from_utf8(bytes)
        .map_err(|e| Error::io(path, io::Error::new(io::ErrorKind::InvalidData, e)))
}

pub(crate) fn write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    fs::write(path, bytes).map_err(|e| Error::io(path, e))
}

pub(crate) fn create_dir_all(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path).map_err(|e| Error::io(path, e))
}

/// Remove a file; a file that is not there is not an error.
pub(crate) fn remove_if_present(path: &Path) -> Result<(), Error> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(Error::io(path, e)),
        _ => Ok(()),
    }
}

/// Every `*.rb` under `dir`, sorted as paths. This is the walk the Ruby
/// frontend does (`ruff_ruby_spo` `parse::walk_rb`), so the file table lists
/// the files the harvest read, in the order it read them. A directory that
/// cannot be read yields nothing, as it does there.
pub(crate) fn ruby_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rb") {
            out.push(path);
        }
    }
}
