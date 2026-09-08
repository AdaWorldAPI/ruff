//! Census the ordered ore over a real Rails tree, so the arm is measured on
//! source it was not written against.
//!
//! `RAILS_SRC=/path/to/openproject cargo run -p ruff_ruby_spo --example ore_census`

#![expect(
    clippy::print_stderr,
    reason = "census CLI example (mirrors ruff_cpp_spo::harvest_events)"
)]

use std::path::PathBuf;

use ruff_ruby_spo::events::{EventKind, Symbols, source_ore};

fn main() {
    let root = PathBuf::from(
        std::env::var("RAILS_SRC").unwrap_or_else(|_| "/home/user/adaworldapi/openproject".into()),
    );
    let limit: usize = std::env::var("LIMIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(usize::MAX);

    let mut files = Vec::new();
    for dir in ["app/models", "app/services", "app/contracts"] {
        collect(&root.join(dir), &mut files);
    }
    files.sort();
    files.truncate(limit);

    let mut syms = Symbols::default();
    let (mut classes, mut methods, mut events, mut scopes, mut decls) =
        (0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut set_total, mut ev_facts) = (0u64, 0u64);
    let mut with_multi_scope = 0u64;
    let mut cb_with_options = 0u64;
    let mut kinds: std::collections::BTreeMap<&'static str, u64> =
        std::collections::BTreeMap::new();

    for path in &files {
        let Ok(src) = std::fs::read_to_string(path) else {
            continue;
        };
        for c in source_ore(&src, "op", &mut syms) {
            classes += 1;
            decls += c.declarations.len() as u64;
            for d in &c.declarations {
                if let ruff_ruby_spo::Declaration::Callback(cb) = &d.decl
                    && !cb.options.is_empty()
                {
                    cb_with_options += 1;
                }
            }
            for m in &c.methods {
                methods += 1;
                events += m.events.len() as u64;
                scopes += m.scopes.len() as u64;
                if m.scopes.len() > 1 {
                    with_multi_scope += 1;
                }
                set_total += u64::from(
                    m.set_reads + m.set_writes + m.set_raises + m.set_calls + m.set_traverses,
                );
                for e in &m.events {
                    *kinds.entry(e.kind.as_str()).or_default() += 1;
                    if matches!(
                        e.kind,
                        EventKind::Read
                            | EventKind::Write
                            | EventKind::ReadWrite
                            | EventKind::Raise
                            | EventKind::Call
                    ) {
                        ev_facts += 1;
                    }
                }
            }
        }
    }

    eprintln!("files            {}", files.len());
    eprintln!("classes          {classes}");
    eprintln!("methods          {methods}");
    eprintln!("declarations     {decls}   (callbacks carrying options: {cb_with_options})");
    eprintln!("events           {events}");
    eprintln!("scopes           {scopes}   (methods with >1 scope: {with_multi_scope})");
    eprintln!("symbols          {}", syms.len());
    eprintln!("--");
    eprintln!("shipped set entries   {set_total}");
    eprintln!("ordered fact events   {ev_facts}");
    if set_total > 0 {
        // `f64::from(u32)` is lossless; the counts are corpus-sized, so the
        // saturating narrowing is unreachable in practice and never silently
        // rounds the way a bare `u64 as f64` would.
        let num = f64::from(u32::try_from(ev_facts).unwrap_or(u32::MAX));
        let den = f64::from(u32::try_from(set_total).unwrap_or(u32::MAX));
        eprintln!("preservation ratio    {:.2}x", num / den);
    }
    eprintln!("--\nevent kinds:");
    for (k, n) in &kinds {
        eprintln!("  {k:<12} {n}");
    }
}

fn collect(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "rb") {
            out.push(p);
        }
    }
}
