//! Decorator selector matching shared by the decorated-function and
//! decorated-class matchers.

use ruff_python_ast::{Decorator, Expr};

use crate::config::DecoratorSelector;

/// Find the first decorator in `decorators` that satisfies `selector`.
pub(crate) fn find_matching_decorator<'a>(
    decorators: &'a [Decorator],
    selector: Option<&DecoratorSelector>,
) -> Option<&'a Decorator> {
    decorators
        .iter()
        .find(|&dec| decorator_matches(dec, selector))
}

/// Check if a decorator satisfies a selector.
pub(crate) fn decorator_matches(dec: &Decorator, sel: Option<&DecoratorSelector>) -> bool {
    let Some(sel) = sel else {
        // No selector → match any decorated definition.
        return true;
    };
    if sel.any {
        return true;
    }

    let min_pos = sel.min_positional_args.unwrap_or(0);

    if let Some(attr_name) = &sel.attribute {
        if decorator_has_attribute(dec, attr_name) {
            return call_has_min_positional(dec, min_pos);
        }
    }

    if let Some(bare_name) = &sel.name {
        if decorator_has_bare_name(dec, bare_name) {
            return call_has_min_positional(dec, min_pos);
        }
    }

    false
}

/// `@bp.route(...)` → attribute = "route"
fn decorator_has_attribute(dec: &Decorator, attr_name: &str) -> bool {
    if let Expr::Call(call) = &dec.expression
        && let Expr::Attribute(attr) = &*call.func
    {
        return attr.attr.id.as_str() == attr_name;
    }
    // Non-call decorator with attribute access: `@bp.route` (no parens).
    if let Expr::Attribute(attr) = &dec.expression {
        return attr.attr.id.as_str() == attr_name;
    }
    false
}

/// `@login_required` → name = "`login_required`"
fn decorator_has_bare_name(dec: &Decorator, name: &str) -> bool {
    if let Expr::Call(call) = &dec.expression
        && let Expr::Name(n) = &*call.func
    {
        return n.id.as_str() == name;
    }
    if let Expr::Name(n) = &dec.expression {
        return n.id.as_str() == name;
    }
    false
}

fn call_has_min_positional(dec: &Decorator, min: u32) -> bool {
    if min == 0 {
        return true;
    }
    if let Expr::Call(call) = &dec.expression {
        return u32::try_from(call.arguments.args.len()).unwrap_or(u32::MAX) >= min;
    }
    false
}
