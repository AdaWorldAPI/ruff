use std::fmt;
use std::io;

/// Everything that can stop a projection or an order question.
#[derive(Debug)]
pub enum Error {
    Io {
        path: String,
        source: io::Error,
    },
    /// A problem in the config file; `message` names the offending key.
    Config {
        path: String,
        message: String,
    },
    /// A cell that cannot be written to a TSV line.
    BadCell {
        what: &'static str,
        value: String,
    },
    /// The imports JSON is not an object of path to array of paths.
    Imports {
        path: String,
        message: String,
    },
    /// A store file that does not have the shape this crate writes.
    Store {
        path: String,
        message: String,
    },
    /// The rows disagree with what the source graph held; nothing was written.
    Conservation(Vec<String>),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{path}: {source}"),
            Self::Config { path, message } => write!(f, "config {path}: {message}"),
            Self::BadCell { what, value } => {
                write!(
                    f,
                    "refused {what} {value:?}: empty, control character or `?` segment"
                )
            }
            Self::Imports { path, message } => write!(f, "imports {path}: {message}"),
            Self::Store { path, message } => write!(f, "store {path}: {message}"),
            Self::Conservation(items) => {
                write!(f, "conservation failed: {}", items.join("; "))
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Error {
    pub(crate) fn io(path: &std::path::Path, source: io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            source,
        }
    }
}
