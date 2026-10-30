//! Matcher for `class_with_decorator` kind.
//!
//! Matches top-level class definitions decorated with a decorator that
//! satisfies the rule's `decorator` selector, then evaluates the `emit`
//! dot-path expressions to produce an [`EmittedBundle`]. Like the function
//! matcher, only module-level statements are visited: classes nested in
//! classes or functions are out of scope. The class name is stored in
//! [`EmittedBundle::function_name`].

use std::collections::BTreeMap;

use ruff_python_ast::{Stmt, StmtClassDef};
use ruff_python_parser::parse_module;
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use serde_json::Value;

use crate::bundle::EmittedBundle;
use crate::config::{Config, MatchKind, MatchRule};
use crate::emit::{ClassContext, eval_class_path};
use crate::matcher::decorator::find_matching_decorator;
use crate::matcher::function_with_decorator::resolve_family;

/// Parse one Python source file and emit one [`EmittedBundle`] per matched
/// `class_with_decorator` rule and class. Rules of other kinds are skipped.
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
        let Stmt::ClassDef(class) = stmt else {
            continue;
        };
        for rule in config
            .match_rules
            .iter()
            .filter(|r| r.kind == MatchKind::ClassWithDecorator)
        {
            if let Some(bundle) =
                try_match_class(source_file, source, &line_index, class, rule, config)
            {
                out.push(bundle);
            }
        }
    }
    out
}

/// Try to match a single class against a single rule.
fn try_match_class(
    source_file: &str,
    source: &str,
    line_index: &LineIndex,
    class: &StmtClassDef,
    rule: &MatchRule,
    config: &Config,
) -> Option<EmittedBundle> {
    let matched_dec = find_matching_decorator(&class.decorator_list, rule.decorator.as_ref())?;

    let ctx = ClassContext {
        class,
        matched_decorator: matched_dec,
        source,
        line_index,
    };

    let mut fields: BTreeMap<String, Value> = BTreeMap::new();
    for (field_name, path) in &rule.emit {
        fields.insert(field_name.clone(), eval_class_path(path, &ctx));
    }

    let line_start =
        u32::try_from(line_index.line_index(class.range().start()).get()).unwrap_or(u32::MAX);
    let line_end =
        u32::try_from(line_index.line_index(class.range().end()).get()).unwrap_or(u32::MAX);

    let all_decorators: Vec<String> = class
        .decorator_list
        .iter()
        .map(|d| source[d.range().to_std_range()].to_string())
        .collect();

    Some(EmittedBundle {
        match_id: rule.id.clone(),
        file: source_file.to_string(),
        function_name: class.name.id.to_string(),
        family: resolve_family(source_file, config),
        line_start,
        line_end,
        body_lines: line_end.saturating_sub(line_start) + 1,
        all_decorators,
        fields,
        comparison_within_family: None,
    })
}
