//! Dot-path expression evaluator over a matched AST function or class context.
//!
//! Supported dot-paths for `function_with_decorator`:
//! - `def.name`              → function name string
//! - `def.params`            → comma-joined parameter names
//! - `def.body.source`       → raw source text of the body
//! - `def.decorators`        → JSON array of decorator raw strings
//! - `def.range.line_start`  → 1-based start line
//! - `def.range.line_end`    → 1-based end line
//! - `decorator.args[<int>]` → positional argument at index
//! - `decorator.kwargs.<k>`  → keyword argument by name
//! - `decorator.raw`         → full decorator text including `@`
//!
//! Supported dot-paths for `class_with_decorator`:
//! - `class.name`            → class name string
//! - `class.bases`           → JSON array of the raw source text of each base
//!   and keyword (`"Base"`, `"metaclass=M"`), in source order
//! - `class.decorators`      → JSON array of decorator raw strings
//! - `class.range.line_start` / `class.range.line_end` → 1-based lines
//! - `class.body.source`     → raw source text of the body
//! - `class.fields`          → JSON array of `{name, annotation, default}` for
//!   each top-level body `AnnAssign` whose target is a bare name; annotation
//!   and default are raw source text (default is `null` when absent).
//!   `ClassVar` and similar annotations are not filtered out.
//! - `class.methods`         → JSON array of top-level `def` names in the body
//! - `decorator.raw`, `decorator.args[<int>]`, `decorator.kwargs.<k>` as above
//!
//! Path is emitted as `null` if absent. Type mismatches emit the raw value
//! and a `_type_note` sibling field — never panics.

use ruff_python_ast::{Decorator, Expr, Stmt, StmtClassDef, StmtFunctionDef};
use ruff_source_file::LineIndex;
use ruff_text_size::Ranged;
use serde_json::Value;

/// All context available when evaluating emit paths for a matched function.
pub struct FunctionContext<'a> {
    pub func: &'a StmtFunctionDef,
    /// The specific decorator that triggered the match rule.
    pub matched_decorator: &'a ruff_python_ast::Decorator,
    pub source: &'a str,
    pub line_index: &'a LineIndex,
}

/// All context available when evaluating emit paths for a matched class.
pub struct ClassContext<'a> {
    pub class: &'a StmtClassDef,
    /// The specific decorator that triggered the match rule.
    pub matched_decorator: &'a Decorator,
    pub source: &'a str,
    pub line_index: &'a LineIndex,
}

/// Evaluate a dot-path expression against a function context.
/// Returns `Value::Null` if the path is absent or evaluates to nothing.
pub fn eval_path(path: &str, ctx: &FunctionContext<'_>) -> Value {
    if let Some(rest) = path.strip_prefix("def.") {
        eval_def_path(rest, ctx)
    } else if let Some(rest) = path.strip_prefix("decorator.") {
        eval_decorator_path(rest, ctx.matched_decorator, ctx.source)
    } else {
        Value::Null
    }
}

fn eval_def_path(path: &str, ctx: &FunctionContext<'_>) -> Value {
    match path {
        "name" => Value::String(ctx.func.name.id.to_string()),
        "params" => {
            let params: Vec<String> = ctx
                .func
                .parameters
                .iter_non_variadic_params()
                .map(|p| p.parameter.name.id.to_string())
                .chain(
                    ctx.func
                        .parameters
                        .vararg
                        .as_ref()
                        .map(|v| format!("*{}", v.name.id)),
                )
                .chain(
                    ctx.func
                        .parameters
                        .kwarg
                        .as_ref()
                        .map(|k| format!("**{}", k.name.id)),
                )
                .collect();
            Value::String(params.join(", "))
        }
        "body.source" => {
            let body_range = ctx.func.range();
            // Body starts after the `def ...:` header — skip to first statement.
            if let Some(first_stmt) = ctx.func.body.first() {
                let start = first_stmt.range().start().to_usize();
                let end = body_range.end().to_usize();
                if start <= end && end <= ctx.source.len() {
                    return Value::String(ctx.source[start..end].to_string());
                }
            }
            Value::Null
        }
        "decorators" => {
            let decs: Vec<Value> = ctx
                .func
                .decorator_list
                .iter()
                .map(|d| {
                    let start = d.range().start().to_usize();
                    let end = d.range().end().to_usize();
                    Value::String(ctx.source[start..end].to_string())
                })
                .collect();
            Value::Array(decs)
        }
        "range.line_start" => {
            let line = ctx.line_index.line_index(ctx.func.range().start()).get();
            Value::Number(line.into())
        }
        "range.line_end" => {
            let line = ctx.line_index.line_index(ctx.func.range().end()).get();
            Value::Number(line.into())
        }
        _ => Value::Null,
    }
}

/// Evaluate a dot-path expression against a class context.
/// Returns `Value::Null` if the path is absent or evaluates to nothing.
pub fn eval_class_path(path: &str, ctx: &ClassContext<'_>) -> Value {
    if let Some(rest) = path.strip_prefix("class.") {
        eval_class_member(rest, ctx)
    } else if let Some(rest) = path.strip_prefix("decorator.") {
        eval_decorator_path(rest, ctx.matched_decorator, ctx.source)
    } else {
        Value::Null
    }
}

fn eval_class_member(path: &str, ctx: &ClassContext<'_>) -> Value {
    let class = ctx.class;
    match path {
        "name" => Value::String(class.name.id.to_string()),
        "bases" => {
            let mut parts: Vec<(usize, String)> = Vec::new();
            if let Some(arguments) = &class.arguments {
                parts.extend(
                    arguments
                        .args
                        .iter()
                        .map(|a| (a.range().start().to_usize(), raw_text(a, ctx.source))),
                );
                parts.extend(
                    arguments
                        .keywords
                        .iter()
                        .map(|k| (k.range().start().to_usize(), raw_text(k, ctx.source))),
                );
            }
            parts.sort_by_key(|(start, _)| *start);
            Value::Array(parts.into_iter().map(|(_, t)| Value::String(t)).collect())
        }
        "body.source" => {
            if let Some(first_stmt) = class.body.first() {
                let start = first_stmt.range().start().to_usize();
                let end = class.range().end().to_usize();
                if start <= end && end <= ctx.source.len() {
                    return Value::String(ctx.source[start..end].to_string());
                }
            }
            Value::Null
        }
        "decorators" => Value::Array(
            class
                .decorator_list
                .iter()
                .map(|d| Value::String(raw_text(d, ctx.source)))
                .collect(),
        ),
        "range.line_start" => {
            let line = ctx.line_index.line_index(class.range().start()).get();
            Value::Number(line.into())
        }
        "range.line_end" => {
            let line = ctx.line_index.line_index(class.range().end()).get();
            Value::Number(line.into())
        }
        "fields" => Value::Array(
            class
                .body
                .iter()
                .filter_map(|stmt| {
                    let Stmt::AnnAssign(ann) = stmt else {
                        return None;
                    };
                    let Expr::Name(target) = &*ann.target else {
                        return None;
                    };
                    let default = ann
                        .value
                        .as_deref()
                        .map_or(Value::Null, |v| Value::String(raw_text(v, ctx.source)));
                    let mut field = serde_json::Map::new();
                    field.insert("name".to_string(), Value::String(target.id.to_string()));
                    field.insert(
                        "annotation".to_string(),
                        Value::String(raw_text(ann.annotation.as_ref(), ctx.source)),
                    );
                    field.insert("default".to_string(), default);
                    Some(Value::Object(field))
                })
                .collect(),
        ),
        "methods" => Value::Array(
            class
                .body
                .iter()
                .filter_map(|stmt| match stmt {
                    Stmt::FunctionDef(f) => Some(Value::String(f.name.id.to_string())),
                    _ => None,
                })
                .collect(),
        ),
        _ => Value::Null,
    }
}

/// Raw source text of a node.
fn raw_text(node: &impl Ranged, source: &str) -> String {
    source[node.range().to_std_range()].to_string()
}

fn eval_decorator_path(path: &str, dec: &Decorator, source: &str) -> Value {
    if path == "raw" {
        return Value::String(raw_text(dec, source));
    }

    // decorator.args[<index>]
    if let Some(rest) = path.strip_prefix("args[") {
        if let Some(idx_str) = rest.strip_suffix(']') {
            if let Ok(idx) = idx_str.parse::<usize>() {
                return eval_decorator_arg(idx, dec, source);
            }
        }
        return Value::Null;
    }

    // decorator.kwargs.<name>
    if let Some(kw_name) = path.strip_prefix("kwargs.") {
        return eval_decorator_kwarg(kw_name, dec, source);
    }

    Value::Null
}

fn eval_decorator_arg(idx: usize, dec: &Decorator, source: &str) -> Value {
    let Expr::Call(call) = &dec.expression else {
        return Value::Null;
    };
    let Some(arg) = call.arguments.args.get(idx) else {
        return Value::Null;
    };
    expr_to_value(arg, source)
}

fn eval_decorator_kwarg(name: &str, dec: &Decorator, source: &str) -> Value {
    let Expr::Call(call) = &dec.expression else {
        return Value::Null;
    };
    for kw in &call.arguments.keywords {
        let Some(kw_name) = kw.arg.as_ref() else {
            continue;
        };
        if kw_name.id.as_str() == name {
            return expr_to_value(&kw.value, source);
        }
    }
    Value::Null
}

/// Convert an AST expression to a JSON value (best-effort, no panic).
fn expr_to_value(expr: &Expr, source: &str) -> Value {
    match expr {
        Expr::StringLiteral(s) => Value::String(s.value.to_str().to_string()),
        Expr::NumberLiteral(n) => match &n.value {
            ruff_python_ast::Number::Int(i) => {
                if let Some(v) = i.as_u64() {
                    Value::Number(v.into())
                } else if let Some(v) = i.as_i64() {
                    Value::Number(v.into())
                } else {
                    Value::String(i.to_string())
                }
            }
            ruff_python_ast::Number::Float(f) => serde_json::Number::from_f64(*f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            ruff_python_ast::Number::Complex { real, imag } => {
                Value::String(format!("{real}+{imag}j"))
            }
        },
        Expr::BooleanLiteral(b) => Value::Bool(b.value),
        Expr::NoneLiteral(_) => Value::Null,
        Expr::List(list) => {
            Value::Array(list.elts.iter().map(|e| expr_to_value(e, source)).collect())
        }
        Expr::Tuple(tup) => {
            Value::Array(tup.elts.iter().map(|e| expr_to_value(e, source)).collect())
        }
        other => {
            // Emit raw source text for anything we can't decompose.
            let start = other.range().start().to_usize();
            let end = other.range().end().to_usize();
            if start <= end && end <= source.len() {
                Value::String(source[start..end].to_string())
            } else {
                Value::Null
            }
        }
    }
}

// Bring in Ranged for the Option-returning arm.
use ruff_text_size::TextRange;

impl FunctionContext<'_> {
    /// Line count of the function body (`line_end` - `line_start` + 1).
    pub fn body_line_count(&self) -> u32 {
        let start = u32::try_from(self.line_index.line_index(self.func.range().start()).get())
            .unwrap_or(u32::MAX);
        let end = u32::try_from(self.line_index.line_index(self.func.range().end()).get())
            .unwrap_or(u32::MAX);
        end.saturating_sub(start) + 1
    }

    /// Raw text of the decorator including `@`.
    pub fn decorator_raw_text(&self) -> &str {
        let start = self.matched_decorator.range().start().to_usize();
        let end = self.matched_decorator.range().end().to_usize();
        &self.source[start..end]
    }
}

// Silence unused import warning — TextRange is used indirectly.
const _: fn() = || {
    let _: TextRange;
};
