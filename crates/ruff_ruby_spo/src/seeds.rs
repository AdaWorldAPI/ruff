//! The Rails seed-call harvest arm: `db/seeds/*.rb` rows as data.
//!
//! A seed file is a list of calls such as
//! `ObjectManager::Attribute.add(name: 'state_id', screens: { ... })` or
//! `Setting.create_if_not_exists(title: 'X', name: 'x', state: { ... })`.
//! Their arguments are hash literals that define which fields a screen shows
//! per role, the default overviews, field rules and setting defaults. This
//! arm reads each matching call as a [`SeedRow`] so the data is harvested,
//! not hand-copied.
//!
//! # AST walk
//!
//! The whole tree is walked, so calls nested in blocks (`each do … end`),
//! `if`/`unless` bodies or argument positions are found. A call matches when
//! its receiver is a constant path (`A::B`, also `::A::B`) equal to the
//! spec's receiver and its method name equals the spec's method. A call on
//! any other receiver (a local, a method result, no receiver) is not a match.
//!
//! # Literal lowering
//!
//! Strings (including `%w[]` / `%i[]` elements, heredocs and adjacent or
//! interpolation-free `dstr` literals), symbols, integers, floats (source
//! text kept), `true`/`false`/`nil`, arrays and hashes lower to
//! [`SeedValue`]. Everything else (method calls, constants, locals,
//! interpolation, ranges, lambdas, `**splat` entries) becomes
//! [`SeedValue::Unresolved`] carrying its exact source text. Nothing is
//! guessed. A hash key that is not a string, symbol, integer, boolean or
//! `nil` literal becomes [`SeedKey::Unresolved`]. A `**splat` hash entry is one unresolved key
//! plus one unresolved value, both holding the splat's source text.
//!
//! A receiver-less call to one of [`SeedOptions::translation_markers`] with a
//! single string literal argument (gettext's `__('Title')`) lowers to
//! [`SeedValue::Translatable`] holding the source string. The markers are
//! configuration: without them such a call stays unresolved.
//!
//! # Conservation
//!
//! In every [`SeedScanReport`]:
//! `calls_found == rows_fully_literal + rows_with_unresolved` and
//! `calls_found == rows.len()`. `unresolved_values` counts every
//! [`SeedValue::Unresolved`] and [`SeedKey::Unresolved`] in all rows. A file
//! that cannot be read or parsed counts in neither `files_scanned` nor any
//! other figure.
//!
//! No SPO triples are emitted: the shared `Predicate` set has no variant for
//! seed rows, and adding one is outside this arm.

use std::fs;
use std::path::Path;

use lib_ruby_parser::nodes::Send as SendNode;
use lib_ruby_parser::traverse::visitor::{Visitor, visit_send};
use lib_ruby_parser::{Node, Parser, ParserOptions};

/// A call to harvest: the constant path of the receiver and the method name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedCallSpec {
    /// Receiver constant path without a leading `::`, e.g. `ObjectManager::Attribute`.
    pub receiver: String,
    /// Method name, e.g. `add`.
    pub method: String,
}

/// A hash key of a seed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedKey {
    Str(String),
    Sym(String),
    Int(i64),
    Bool(bool),
    Nil,
    /// A key that is not one of the literals above; `source` is its text.
    Unresolved {
        source: String,
    },
}

/// A literal value of a seed row, or the exact text of what is not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedValue {
    Str(String),
    Sym(String),
    Int(i64),
    /// A float literal, kept as source text.
    Float(String),
    Bool(bool),
    Nil,
    Array(Vec<SeedValue>),
    Hash(Vec<(SeedKey, SeedValue)>),
    /// A string marked for translation (`__('Title')`), holding the source
    /// string, untranslated.
    Translatable(String),
    /// Any other expression; `source` is its text from the byte range.
    Unresolved {
        source: String,
    },
}

/// One matched call, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRow {
    pub call: SeedCallSpec,
    pub file: String,
    /// 1-based line of the call.
    pub line: u32,
    /// Positional arguments; a trailing keyword-argument list is one `Hash`.
    pub args: Vec<SeedValue>,
}

/// Counts for one [`extract_seed_rows`] run. See the module doc for the
/// conservation law.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SeedScanReport {
    pub files_scanned: usize,
    pub calls_found: usize,
    pub rows_fully_literal: usize,
    pub rows_with_unresolved: usize,
    pub unresolved_values: usize,
}

/// How literal values are lowered beyond plain Ruby literals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeedOptions {
    /// Receiver-less method names that mark a string for translation
    /// (`["__"]` for gettext-style seeds).
    pub translation_markers: Vec<String>,
}

/// Read every file in `files` (paths relative to `root`) and return one
/// [`SeedRow`] per call matching any of `calls`, files in the given order and
/// calls in source order within a file.
#[must_use]
pub fn extract_seed_rows(
    root: &Path,
    files: &[&str],
    calls: &[SeedCallSpec],
) -> (Vec<SeedRow>, SeedScanReport) {
    extract_seed_rows_with(root, files, calls, &SeedOptions::default())
}

/// [`extract_seed_rows`] with lowering options.
#[must_use]
pub fn extract_seed_rows_with(
    root: &Path,
    files: &[&str],
    calls: &[SeedCallSpec],
    options: &SeedOptions,
) -> (Vec<SeedRow>, SeedScanReport) {
    let mut rows = Vec::new();
    let mut report = SeedScanReport::default();
    for rel in files {
        let Ok(src) = fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let parser_options = ParserOptions {
            buffer_name: (*rel).to_string(),
            ..Default::default()
        };
        let Some(ast) = Parser::new(src.as_bytes().to_vec(), parser_options)
            .do_parse()
            .ast
        else {
            continue;
        };
        report.files_scanned += 1;
        let mut collector = Collector {
            lowerer: Lowerer { src: &src, options },
            file: rel,
            specs: calls,
            rows: Vec::new(),
        };
        collector.visit(&ast);
        for row in collector.rows {
            let unresolved = row.args.iter().map(count_unresolved).sum::<usize>();
            report.calls_found += 1;
            if unresolved == 0 {
                report.rows_fully_literal += 1;
            } else {
                report.rows_with_unresolved += 1;
            }
            report.unresolved_values += unresolved;
            rows.push(row);
        }
    }
    (rows, report)
}

struct Collector<'a> {
    lowerer: Lowerer<'a>,
    file: &'a str,
    specs: &'a [SeedCallSpec],
    rows: Vec<SeedRow>,
}

impl Visitor for Collector<'_> {
    fn on_send(&mut self, node: &SendNode) {
        if let Some(recv) = node.recv.as_deref()
            && let Some(path) = const_path(recv)
            && let Some(spec) = self
                .specs
                .iter()
                .find(|s| s.receiver == path && s.method == node.method_name)
        {
            self.rows.push(SeedRow {
                call: spec.clone(),
                file: self.file.to_string(),
                line: self.lowerer.line_of(node.expression_l.begin),
                args: node.args.iter().map(|a| self.lowerer.lower(a)).collect(),
            });
        }
        visit_send(self, node);
    }
}

/// Source-aware lowering of AST nodes to [`SeedValue`]s, shared with the
/// class-macro arm.
pub(crate) struct Lowerer<'a> {
    pub(crate) src: &'a str,
    pub(crate) options: &'a SeedOptions,
}

impl Lowerer<'_> {
    pub(crate) fn line_of(&self, offset: usize) -> u32 {
        let newlines = self
            .src
            .as_bytes()
            .iter()
            .take(offset)
            .filter(|b| **b == b'\n')
            .count();
        u32::try_from(newlines + 1).unwrap_or(u32::MAX)
    }

    pub(crate) fn source_of(&self, node: &Node) -> String {
        let loc = node.expression();
        self.src
            .as_bytes()
            .get(loc.begin..loc.end)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }

    fn unresolved(&self, node: &Node) -> SeedValue {
        SeedValue::Unresolved {
            source: self.source_of(node),
        }
    }

    pub(crate) fn lower(&self, node: &Node) -> SeedValue {
        match node {
            Node::Str(s) => SeedValue::Str(s.value.to_string_lossy()),
            Node::Dstr(d) => self.concat_parts(node, &d.parts),
            Node::Heredoc(h) => self.concat_parts(node, &h.parts),
            Node::Sym(s) => SeedValue::Sym(s.name.to_string_lossy()),
            Node::Int(i) => {
                parse_int(&i.value).map_or_else(|| self.unresolved(node), SeedValue::Int)
            }
            Node::Float(f) => SeedValue::Float(f.value.clone()),
            Node::True(_) => SeedValue::Bool(true),
            Node::False(_) => SeedValue::Bool(false),
            Node::Nil(_) => SeedValue::Nil,
            Node::Array(a) => SeedValue::Array(a.elements.iter().map(|e| self.lower(e)).collect()),
            Node::Hash(h) => self.lower_pairs(&h.pairs),
            Node::Kwargs(k) => self.lower_pairs(&k.pairs),
            Node::Send(call)
                if call.recv.is_none()
                    && self.options.translation_markers.contains(&call.method_name)
                    && let [arg] = call.args.as_slice()
                    && let SeedValue::Str(text) = self.lower(arg) =>
            {
                SeedValue::Translatable(text)
            }
            _ => self.unresolved(node),
        }
    }

    /// A `dstr`/heredoc is a literal only when every part is a plain string.
    fn concat_parts(&self, whole: &Node, parts: &[Node]) -> SeedValue {
        let mut text = String::new();
        for part in parts {
            match part {
                Node::Str(s) => text.push_str(&s.value.to_string_lossy()),
                Node::Dstr(d) => match self.concat_parts(part, &d.parts) {
                    SeedValue::Str(inner) => text.push_str(&inner),
                    _ => return self.unresolved(whole),
                },
                _ => return self.unresolved(whole),
            }
        }
        SeedValue::Str(text)
    }

    fn lower_pairs(&self, pairs: &[Node]) -> SeedValue {
        let mut out = Vec::with_capacity(pairs.len());
        for pair in pairs {
            if let Node::Pair(p) = pair {
                let key = match p.key.as_ref() {
                    Node::Sym(s) => SeedKey::Sym(s.name.to_string_lossy()),
                    Node::Str(s) => SeedKey::Str(s.value.to_string_lossy()),
                    Node::True(_) => SeedKey::Bool(true),
                    Node::False(_) => SeedKey::Bool(false),
                    Node::Nil(_) => SeedKey::Nil,
                    Node::Int(i) if let Some(n) = parse_int(&i.value) => SeedKey::Int(n),
                    other => SeedKey::Unresolved {
                        source: self.source_of(other),
                    },
                };
                out.push((key, self.lower(&p.value)));
            } else {
                let source = self.source_of(pair);
                out.push((
                    SeedKey::Unresolved {
                        source: source.clone(),
                    },
                    SeedValue::Unresolved { source },
                ));
            }
        }
        SeedValue::Hash(out)
    }
}

/// `A`, `A::B`, `::A::B` as `A`, `A::B`, `A::B`; `None` for any other receiver.
pub(crate) fn const_path(node: &Node) -> Option<String> {
    let Node::Const(c) = node else { return None };
    match c.scope.as_deref() {
        None | Some(Node::Cbase(_)) => Some(c.name.clone()),
        Some(scope) => Some(format!("{}::{}", const_path(scope)?, c.name)),
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let cleaned: String = text.chars().filter(|c| *c != '_').collect();
    cleaned.parse().ok()
}

pub(crate) fn count_unresolved(value: &SeedValue) -> usize {
    match value {
        SeedValue::Unresolved { .. } => 1,
        SeedValue::Array(items) => items.iter().map(count_unresolved).sum(),
        SeedValue::Hash(pairs) => pairs
            .iter()
            .map(|(k, v)| {
                usize::from(matches!(k, SeedKey::Unresolved { .. })) + count_unresolved(v)
            })
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch_dir(case: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ruff_ruby_spo_seeds_{}_{case}", std::process::id()))
    }

    fn spec(receiver: &str, method: &str) -> SeedCallSpec {
        SeedCallSpec {
            receiver: receiver.to_string(),
            method: method.to_string(),
        }
    }

    fn scan(case: &str, body: &str, calls: &[SeedCallSpec]) -> (Vec<SeedRow>, SeedScanReport) {
        let root = scratch_dir(case);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("seed.rb"), body).unwrap();
        let result = extract_seed_rows(&root, &["seed.rb"], calls);
        let _ = fs::remove_dir_all(&root);
        result
    }

    fn sym(s: &str) -> SeedKey {
        SeedKey::Sym(s.to_string())
    }

    fn string(s: &str) -> SeedValue {
        SeedValue::Str(s.to_string())
    }

    fn only_kwargs(row: &SeedRow) -> &Vec<(SeedKey, SeedValue)> {
        assert_eq!(row.args.len(), 1);
        let SeedValue::Hash(pairs) = &row.args[0] else {
            panic!("expected a hash, got {:?}", row.args[0]);
        };
        pairs
    }

    #[test]
    fn matches_receiver_and_lowers_fields() {
        let body = "Setting.create_if_not_exists(title: 'A', name: 'a', state: { value: true })\n\
                    Other.create_if_not_exists(title: 'X')\n\
                    Setting.create_if_not_exists(title: 'B', name: 'b', state: { value: true })\n";
        let (rows, report) = scan("t1", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(rows.len(), 2);
        assert_eq!(
            report,
            SeedScanReport {
                files_scanned: 1,
                calls_found: 2,
                rows_fully_literal: 2,
                rows_with_unresolved: 0,
                unresolved_values: 0,
            }
        );
        assert_eq!(rows[0].line, 1);
        assert_eq!(rows[1].line, 3);
        assert_eq!(
            rows[0].args,
            vec![SeedValue::Hash(vec![
                (sym("title"), string("A")),
                (sym("name"), string("a")),
                (
                    sym("state"),
                    SeedValue::Hash(vec![(sym("value"), SeedValue::Bool(true))])
                ),
            ])]
        );
        assert_eq!(only_kwargs(&rows[1])[1], (sym("name"), string("b")));
    }

    #[test]
    fn non_literal_nested_value_is_unresolved_with_source() {
        let body = "ObjectManager::Attribute.add(name: 'x', data_option: { options: Ticket::State.by_category(:open).pluck(:id) })\n";
        let (rows, report) = scan("t2", body, &[spec("ObjectManager::Attribute", "add")]);
        assert_eq!(rows.len(), 1);
        assert_eq!(report.calls_found, 1);
        assert_eq!(report.rows_fully_literal, 0);
        assert_eq!(report.rows_with_unresolved, 1);
        assert_eq!(report.unresolved_values, 1);
        assert_eq!(
            only_kwargs(&rows[0])[1],
            (
                sym("data_option"),
                SeedValue::Hash(vec![(
                    sym("options"),
                    SeedValue::Unresolved {
                        source: "Ticket::State.by_category(:open).pluck(:id)".to_string()
                    }
                )])
            )
        );
    }

    #[test]
    fn a_configured_translation_marker_lowers_to_a_translatable_string() {
        let root = scratch_dir("t7");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("seed.rb"),
            "Setting.create_if_not_exists(title: __('Title'), help: __(name), other: x.__('y'))\n",
        )
        .unwrap();
        let calls = [spec("Setting", "create_if_not_exists")];
        let marked = SeedOptions {
            translation_markers: vec!["__".to_string()],
        };
        let (with, with_report) = extract_seed_rows_with(&root, &["seed.rb"], &calls, &marked);
        let (without, without_report) = extract_seed_rows(&root, &["seed.rb"], &calls);
        let _ = fs::remove_dir_all(&root);

        let kw = only_kwargs(&with[0]);
        assert_eq!(
            kw[0],
            (sym("title"), SeedValue::Translatable("Title".to_string()))
        );
        assert_eq!(
            kw[1],
            (
                sym("help"),
                SeedValue::Unresolved {
                    source: "__(name)".to_string()
                }
            )
        );
        assert_eq!(
            kw[2],
            (
                sym("other"),
                SeedValue::Unresolved {
                    source: "x.__('y')".to_string()
                }
            )
        );
        assert_eq!(with_report.unresolved_values, 2);
        assert_eq!(
            only_kwargs(&without[0])[0],
            (
                sym("title"),
                SeedValue::Unresolved {
                    source: "__('Title')".to_string()
                }
            )
        );
        assert_eq!(without_report.unresolved_values, 3);
    }

    #[test]
    fn integer_boolean_and_nil_hash_keys_are_literal() {
        let body = "Setting.create_if_not_exists(name: 'x', options: { true => 'yes', false => 'no', 10 => 'ten', nil => '-', k => 1 })\n";
        let (rows, report) = scan("t8", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(
            only_kwargs(&rows[0])[1],
            (
                sym("options"),
                SeedValue::Hash(vec![
                    (SeedKey::Bool(true), string("yes")),
                    (SeedKey::Bool(false), string("no")),
                    (SeedKey::Int(10), string("ten")),
                    (SeedKey::Nil, string("-")),
                    (
                        SeedKey::Unresolved {
                            source: "k".to_string()
                        },
                        SeedValue::Int(1)
                    ),
                ])
            )
        );
        assert_eq!(report.unresolved_values, 1);
    }

    #[test]
    fn finds_calls_in_blocks_and_conditionals() {
        let body = "[1, 2].each do |i|\n  Setting.create_if_not_exists(name: 'in_each')\nend\n\
                    if true\n  Setting.create_if_not_exists(name: 'in_if')\nend\n\
                    Setting.create_if_not_exists(name: 'top')\n";
        let (rows, report) = scan("t3", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(report.calls_found, 3);
        let lines: Vec<u32> = rows.iter().map(|r| r.line).collect();
        assert_eq!(lines, vec![2, 5, 7]);
    }

    #[test]
    fn lowers_word_arrays_heredocs_and_negative_integers() {
        let body = "Setting.create_if_not_exists(\n  words: %w[a b],\n  syms: %i[a b],\n  neg: -3,\n  ratio: 1.5,\n  none: nil,\n  adjacent: 'a' 'b',\n  text: <<~TXT,\n    hello\n    world\n  TXT\n  last: 1,\n)\n";
        let (rows, report) = scan("t4", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(report.unresolved_values, 0);
        let pairs = only_kwargs(&rows[0]);
        assert_eq!(pairs[0].1, SeedValue::Array(vec![string("a"), string("b")]));
        assert_eq!(
            pairs[1].1,
            SeedValue::Array(vec![
                SeedValue::Sym("a".to_string()),
                SeedValue::Sym("b".to_string())
            ])
        );
        assert_eq!(pairs[2].1, SeedValue::Int(-3));
        assert_eq!(pairs[3].1, SeedValue::Float("1.5".to_string()));
        assert_eq!(pairs[4].1, SeedValue::Nil);
        assert_eq!(pairs[5].1, string("ab"));
        assert_eq!(pairs[6].1, string("hello\nworld\n"));
        assert_eq!(pairs[7].1, SeedValue::Int(1));
    }

    #[test]
    fn leading_colons_match_and_non_constant_receivers_do_not() {
        let body = "::Setting.create_if_not_exists(name: 'a')\n\
                    setting.create_if_not_exists(name: 'b')\n\
                    Foo::Setting.create_if_not_exists(name: 'c')\n\
                    create_if_not_exists(name: 'd')\n";
        let (rows, report) = scan("t5", body, &[spec("Setting", "create_if_not_exists")]);
        assert_eq!(report.calls_found, 1);
        assert_eq!(rows[0].line, 1);
    }

    #[test]
    fn hash_rocket_keys_and_interpolation() {
        let body = "Setting.create_if_not_exists('k' => 1, :s => \"a#{x}\", y => 2)\n";
        let (rows, report) = scan("t6", body, &[spec("Setting", "create_if_not_exists")]);
        let pairs = only_kwargs(&rows[0]);
        assert_eq!(pairs[0], (SeedKey::Str("k".to_string()), SeedValue::Int(1)));
        assert_eq!(
            pairs[1],
            (
                sym("s"),
                SeedValue::Unresolved {
                    source: "\"a#{x}\"".to_string()
                }
            )
        );
        assert_eq!(
            pairs[2].0,
            SeedKey::Unresolved {
                source: "y".to_string()
            }
        );
        assert_eq!(report.unresolved_values, 2);
        assert_eq!(
            report.calls_found,
            report.rows_fully_literal + report.rows_with_unresolved
        );
    }

    /// Run with `ZAMMAD_SRC=/path/to/zammad cargo test -p ruff_ruby_spo
    /// seeds::tests::zammad_seed_measurement -- --ignored --nocapture`.
    #[test]
    #[ignore = "needs ZAMMAD_SRC"]
    #[expect(
        clippy::print_stdout,
        reason = "measurement output is the purpose of this test"
    )]
    fn zammad_seed_measurement() {
        let Ok(src) = std::env::var("ZAMMAD_SRC") else {
            return;
        };
        let root = Path::new(&src);
        let targets = [
            (
                "ObjectManager::Attribute",
                "add",
                "db/seeds/object_manager_attributes.rb",
            ),
            ("Overview", "create_if_not_exists", "db/seeds/overviews.rb"),
            (
                "CoreWorkflow",
                "create_if_not_exists",
                "db/seeds/core_workflow.rb",
            ),
            ("Setting", "create_if_not_exists", "db/seeds/settings.rb"),
        ];
        let mut shapes: std::collections::BTreeMap<String, (usize, String)> =
            std::collections::BTreeMap::new();
        for (receiver, method, file) in targets {
            let options = SeedOptions {
                translation_markers: vec!["__".to_string()],
            };
            let (rows, report) =
                extract_seed_rows_with(root, &[file], &[spec(receiver, method)], &options);
            println!("{receiver}.{method} {file}: {report:?}");
            assert_eq!(rows.len(), report.calls_found);
            for row in &rows {
                for arg in &row.args {
                    collect_shapes(arg, &row.file, row.line, &mut shapes);
                }
            }
        }
        let mut ranked: Vec<_> = shapes.into_iter().collect();
        ranked.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
        for (shape, (count, example)) in ranked.iter().take(5) {
            println!("{count:5}  {shape}  e.g. {example}");
        }
    }

    fn collect_shapes(
        value: &SeedValue,
        file: &str,
        line: u32,
        out: &mut std::collections::BTreeMap<String, (usize, String)>,
    ) {
        match value {
            SeedValue::Unresolved { source } => {
                let shape: String = source
                    .chars()
                    .take_while(|c| !matches!(c, '(' | '.' | ' '))
                    .collect();
                let entry = out.entry(shape).or_insert((0, format!("{file}:{line}")));
                entry.0 += 1;
            }
            SeedValue::Array(items) => {
                for item in items {
                    collect_shapes(item, file, line, out);
                }
            }
            SeedValue::Hash(pairs) => {
                for (_, v) in pairs {
                    collect_shapes(v, file, line, out);
                }
            }
            _ => {}
        }
    }
}
