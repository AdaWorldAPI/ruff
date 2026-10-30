//! Config-driven matchers and their combined entry point.

pub mod class_with_decorator;
pub(crate) mod decorator;
pub mod function_with_decorator;

use crate::bundle::EmittedBundle;
use crate::config::Config;

/// Run every matcher kind over one source file and return the bundles in
/// source order (stable sort on `line_start`).
pub fn harvest_with_config(source_file: &str, source: &str, config: &Config) -> Vec<EmittedBundle> {
    let mut out = function_with_decorator::harvest_module_with_config(source_file, source, config);
    out.extend(class_with_decorator::harvest_module_with_config(
        source_file,
        source,
        config,
    ));
    out.sort_by_key(|b| b.line_start);
    out
}
