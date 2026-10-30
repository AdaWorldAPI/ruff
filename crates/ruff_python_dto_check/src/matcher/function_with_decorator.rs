//! Matcher for `function_with_decorator` kind.
//!
//! Matches top-level function definitions decorated with a decorator that
//! satisfies the rule's `decorator` selector, then evaluates the `emit`
//! dot-path expressions to produce an [`EmittedBundle`].

use std::collections::BTreeMap;
use std::path::Path;

use ruff_python_ast::{Stmt, StmtFunctionDef};
use ruff_python_parser::parse_module;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use serde_json::Value;

use crate::bundle::EmittedBundle;
use crate::config::{Config, MatchKind, MatchRule};
use crate::emit::{FunctionContext, eval_path};
use crate::matcher::decorator::find_matching_decorator;

/// Parse one Python source file and emit one [`EmittedBundle`] per matched
/// `function_with_decorator` rule and function. Rules of other kinds are skipped.
pub fn harvest_module_with_config(
    source_file: &str,
    source: &str,
    config: &Config,
) -> Vec<EmittedBundle> {
    let Ok(parsed) = parse_module(source) else {
        return Vec::new();
    };
    let line_index = LineIndex::from_source_text(source);
    let mut out = Vec::new();

    for stmt in &parsed.syntax().body {
        let Stmt::FunctionDef(func) = stmt else {
            continue;
        };
        for rule in config
            .match_rules
            .iter()
            .filter(|r| r.kind == MatchKind::FunctionWithDecorator)
        {
            if let Some(bundle) =
                try_match_function(source_file, source, &line_index, func, rule, config)
            {
                out.push(bundle);
            }
        }
    }
    out
}

/// Try to match a single function against a single rule.
fn try_match_function(
    source_file: &str,
    source: &str,
    line_index: &LineIndex,
    func: &StmtFunctionDef,
    rule: &MatchRule,
    config: &Config,
) -> Option<EmittedBundle> {
    let matched_dec = find_matching_decorator(&func.decorator_list, rule.decorator.as_ref())?;

    let ctx = FunctionContext {
        func,
        matched_decorator: matched_dec,
        source,
        line_index,
    };

    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    for (field_name, path) in &rule.emit {
        let val = eval_path(path, &ctx);
        fields.insert(field_name.clone(), val);
    }

    let function_name = func.name.id.to_string();
    let family = resolve_family(source_file, config);

    let line_start =
        u32::try_from(line_index.line_index(func.range().start()).get()).unwrap_or(u32::MAX);
    let line_end =
        u32::try_from(line_index.line_index(func.range().end()).get()).unwrap_or(u32::MAX);
    let body_lines = line_end.saturating_sub(line_start) + 1;

    let all_decorators: Vec<String> = func
        .decorator_list
        .iter()
        .map(|d| {
            let start = d.range().start().to_usize();
            let end = d.range().end().to_usize();
            source[start..end].to_string()
        })
        .collect();

    Some(EmittedBundle {
        match_id: rule.id.clone(),
        file: source_file.to_string(),
        function_name,
        family,
        line_start,
        line_end,
        body_lines,
        all_decorators,
        fields,
        comparison_within_family: None,
    })
}

/// Resolve family name from `source_file` path using the config's group rule.
pub fn resolve_family(source_file: &str, config: &Config) -> String {
    let filename = Path::new(source_file)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    if let Some(fff) = &config.group.family_from_filename {
        if let Some(caps) = fff.compiled.captures(filename) {
            if let Some(m) = caps.name("family") {
                return m.as_str().to_string();
            }
        }
    }

    // Fallback: stem without extension.
    Path::new(source_file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}
