use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::Error;
use crate::fsio;

/// The source frontends this crate can run. One so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frontend {
    RubyRails,
}

impl Frontend {
    const SUPPORTED: &'static str = "ruby-rails";

    fn parse(text: &str) -> Option<Self> {
        (text == "ruby-rails").then_some(Self::RubyRails)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    namespace: String,
    frontend: String,
    root: String,
    #[serde(default)]
    lanes: RawLanes,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLanes {
    #[serde(default = "yes")]
    structural: bool,
    #[serde(default = "yes")]
    order: bool,
    imports: Option<String>,
}

impl Default for RawLanes {
    fn default() -> Self {
        Self {
            structural: true,
            order: true,
            imports: None,
        }
    }
}

fn yes() -> bool {
    true
}

/// A validated run description.
#[derive(Debug, Clone)]
pub struct Config {
    pub namespace: String,
    pub frontend: Frontend,
    /// The source root as written in the config, normalised to forward
    /// slashes without a leading `./`. File paths in the store start with it.
    pub root_rel: String,
    /// The directory the source root resolves to.
    pub root: PathBuf,
    pub structural: bool,
    pub order: bool,
    /// The imports JSON, resolved against the config file's directory.
    pub imports: Option<PathBuf>,
}

impl Config {
    /// Read and validate a config file. Unknown keys, an unknown frontend, an
    /// unusable namespace and a root outside the config directory are errors
    /// that name the key.
    pub fn load(path: &Path) -> Result<Self, Error> {
        let shown = path.display().to_string();
        let fail = |message: String| Error::Config {
            path: shown.clone(),
            message,
        };
        let text = fsio::read_text(path)?;
        let raw: RawConfig = toml::from_str(&text).map_err(|e| fail(e.to_string()))?;

        let frontend = Frontend::parse(&raw.frontend).ok_or_else(|| {
            fail(format!(
                "key `frontend`: unknown frontend {:?} (supported: {})",
                raw.frontend,
                Frontend::SUPPORTED
            ))
        })?;
        if raw.namespace.is_empty()
            || !raw
                .namespace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(fail(format!(
                "key `namespace`: {:?} must be non-empty and use only letters, digits, `_` and `-`",
                raw.namespace
            )));
        }
        let root_rel = relative_inside(&raw.root).ok_or_else(|| {
            fail(format!(
                "key `root`: {:?} must be a relative path inside the config directory",
                raw.root
            ))
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        let root = base.join(&root_rel);
        let imports = match raw.lanes.imports.as_deref() {
            None => None,
            Some(p) => {
                relative_inside(p).ok_or_else(|| {
                    fail(format!("key `lanes.imports`: {p:?} must be a relative path inside the config directory"))
                })?;
                Some(base.join(p))
            }
        };
        Ok(Self {
            namespace: raw.namespace,
            frontend,
            root_rel,
            root,
            structural: raw.lanes.structural,
            order: raw.lanes.order,
            imports,
        })
    }
}

/// `p` as a forward-slash relative path without `.` components, or `None`
/// when it is absolute, empty or climbs out with `..`.
fn relative_inside(p: &str) -> Option<String> {
    let mut parts = Vec::new();
    for component in Path::new(p).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}
