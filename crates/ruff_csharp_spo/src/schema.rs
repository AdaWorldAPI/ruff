//! **Schema stratum** — physical DB columns from a `MySQL` `mysqldump
//! --no-data` structure export.
//!
//! The Roslyn harvester (`harvester/Program.cs`) lifts C# property types as
//! written in source; it never reads the database's own DDL. That means a
//! property whose C# type is a loosely-typed wrapper (or whose nullability
//! is only expressed in the schema, not the class) never reaches the
//! [`ruff_spo_triplet::ModelGraph`] with a `field_type` / `not_null` fact.
//! [`extract_schema`] closes that gap the same way
//! [`ruff_ruby_spo::schema::extract_app_with_schema`] does for Rails: parse
//! the DB-truth structure directly and land it as [`Field`]s.
//!
//! Unlike the Rails baseline (one `Tables::X` class per table, matched by
//! inflection onto an already-harvested `ActiveRecord` class), a `mysqldump`
//! structure export carries no separate class-body harvest to merge into —
//! **the table IS the model**, verbatim. So [`extract_schema`] builds its
//! own [`ModelGraph`] directly, one [`Model`] per `CREATE TABLE` block, table
//! name unchanged (no Rails-style inflection).
//!
//! # Scope (recorded honestly, conservation-ledger style)
//!
//! - **Zero relation fabrication.** A structure-only dump's `CONSTRAINT …
//!   FOREIGN KEY` lines are constraint facts, not `has_field`-shaped
//!   relations this module trusts — they are skipped (recognised, not
//!   silently dropped) and every [`Field`] here carries `target`,
//!   `relation_kind`, and `inverse_name` all `None`. Convention-based FK
//!   inference (`customer_id` → `customers.id`) is a later, measured slice,
//!   same fence the ruby schema stratum draws around incremental migrations.
//! - **Baseline structure only.** The export is assumed to already be the
//!   target-state DDL (`mysqldump`'s normal `--no-data` output); there is no
//!   `ALTER TABLE` replay.
//! - `PRIMARY KEY` / `UNIQUE KEY` / `KEY` / `CONSTRAINT` / `FOREIGN KEY` /
//!   `FULLTEXT` / `SPATIAL` / `CHECK` lines are index/constraint facts, not
//!   columns — recognised and skipped here (a later slice can lift them).
//! - Any other line inside a `CREATE TABLE` block that is neither a
//!   `` `column` `` declaration nor one of the recognised skip-constructs
//!   lands in [`SchemaReport::unmatched`] — nothing drops silently.

use std::fs;
use std::path::Path;

use ruff_spo_triplet::{Field, Model, ModelGraph};

/// Conservation-ledger report for [`extract_schema`] — mirrors
/// `ruff_ruby_spo::schema::SchemaReport`'s "nothing drops silently" ethos.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaReport {
    /// `CREATE TABLE` blocks detected in the structure export.
    pub tables_seen: usize,
    /// Tables whose block closed cleanly and became a [`Model`].
    pub tables_built: usize,
    /// Lines that could not be classified as a column declaration or a
    /// recognised index/key/constraint skip-construct — named, never
    /// silently dropped. Also carries a table whose `CREATE TABLE` block
    /// never found its closing `)`.
    pub unmatched: Vec<String>,
    /// Provenance marker: which export surface produced the columns.
    pub columns_from: &'static str,
}

/// The base SQL column-type keywords this module recognises by name.
/// Unknown types still parse — a column's `field_type` carries whatever
/// keyword the dump used verbatim — this list is a closed-vocabulary
/// reference for callers/tests, not a gate.
pub const SQL_COLUMN_TYPES: &[&str] = &[
    "int",
    "bigint",
    "smallint",
    "mediumint",
    "tinyint",
    "varchar",
    "char",
    "text",
    "mediumtext",
    "longtext",
    "datetime",
    "date",
    "time",
    "timestamp",
    "float",
    "double",
    "decimal",
    "blob",
    "longblob",
    "enum",
    "set",
];

/// The first token of a line inside a `CREATE TABLE` block that marks it as
/// an index / key / constraint fact rather than a column declaration.
const SKIP_FIRST_WORDS: &[&str] = &[
    "PRIMARY",
    "UNIQUE",
    "KEY",
    "CONSTRAINT",
    "FOREIGN",
    "FULLTEXT",
    "SPATIAL",
    "INDEX",
    "CHECK",
];

/// Parse a `MySQL` `mysqldump --no-data` structure export into a
/// [`ModelGraph`]: one [`Model`] per `CREATE TABLE` block, table name
/// verbatim, columns as schema-typed [`Field`]s. See the module docs for
/// the honest fence (no relation fabrication, baseline structure only).
#[must_use]
pub fn extract_schema(struktur_sql: &Path, namespace: &str) -> (ModelGraph, SchemaReport) {
    let src = fs::read_to_string(struktur_sql).unwrap_or_default();
    extract_schema_str(&src, namespace)
}

/// The text-based core of [`extract_schema`] — kept separate so tests can
/// exercise the parser against an inline fixture with no filesystem I/O
/// (mirroring `ruff_ruby_spo::schema::parse_table_source`'s split).
pub(crate) fn extract_schema_str(src: &str, namespace: &str) -> (ModelGraph, SchemaReport) {
    let mut graph = ModelGraph::new(namespace);
    let mut report = SchemaReport {
        columns_from: "mysqldump-structure-export",
        ..SchemaReport::default()
    };

    for (name, fields) in parse_dump(src, &mut report) {
        report.tables_built += 1;
        graph.models.push(Model {
            name,
            fields,
            ..Model::default()
        });
    }

    (graph, report)
}

/// Scan the whole structure export for `CREATE TABLE` blocks, reporting
/// unclassifiable lines into `report.unmatched` as they're found.
fn parse_dump(src: &str, report: &mut SchemaReport) -> Vec<(String, Vec<Field>)> {
    let mut tables = Vec::new();
    let mut lines = src.lines();

    while let Some(raw) = lines.next() {
        let Some(name) = create_table_name(raw.trim()) else {
            continue;
        };
        report.tables_seen += 1;

        let mut fields = Vec::new();
        let mut closed = false;
        for raw in lines.by_ref() {
            let line = raw.trim();
            if line.starts_with(')') {
                closed = true;
                break;
            }
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('`') {
                match parse_column_line(rest) {
                    Some(field) => fields.push(field),
                    None => report
                        .unmatched
                        .push(format!("{name}: unparseable column line: {line}")),
                }
            } else if is_skip_construct(line) {
                // Index / key / constraint fact, not a column — recognised,
                // deliberately not lifted (see the module's honest fence).
            } else {
                report
                    .unmatched
                    .push(format!("{name}: unrecognised line: {line}"));
            }
        }

        if closed {
            tables.push((name, fields));
        } else {
            report
                .unmatched
                .push(format!("{name}: CREATE TABLE block never closed"));
        }
    }

    tables
}

/// `CREATE TABLE` (optionally with `IF NOT EXISTS`) followed by a
/// backtick-quoted table name and an opening paren → the table name.
/// `None` for any other line.
fn create_table_name(line: &str) -> Option<String> {
    let rest = line.strip_prefix("CREATE TABLE ")?;
    let rest = rest
        .strip_prefix("IF NOT EXISTS ")
        .unwrap_or(rest)
        .trim_start();
    let rest = rest.strip_prefix('`')?;
    let end = rest.find('`')?;
    Some(rest[..end].to_string())
}

/// Parse one column-declaration line, given the text **after** its opening
/// backtick (the caller already stripped it). `` `name` <type>[(len)] […] ``
/// → a [`Field`] with `field_type` = the base type keyword (parens/args
/// stripped, lowercased) and `not_null = Some(true)` iff the line contains
/// `NOT NULL`, else `None`. `None` when the closing backtick around the
/// column name is missing (malformed line — the caller reports it).
fn parse_column_line(after_backtick: &str) -> Option<Field> {
    let end = after_backtick.find('`')?;
    let name = &after_backtick[..end];
    let rest = after_backtick[end + 1..].trim();
    // Only the trailing comma (if any) is a separator — an internal comma,
    // e.g. inside `decimal(10,2)`, is not at the string's end and survives.
    let rest = rest.strip_suffix(',').unwrap_or(rest).trim();

    let raw_type = rest.split_whitespace().next()?;
    let base_type = raw_type
        .split('(')
        .next()
        .unwrap_or(raw_type)
        .to_lowercase();
    let not_null = rest.contains("NOT NULL");

    Some(Field {
        name: name.to_string(),
        field_type: Some(base_type),
        not_null: if not_null { Some(true) } else { None },
        ..Field::default()
    })
}

/// Whether `line` (trimmed, backtick-stripped check already failed) is a
/// recognised index / key / constraint fact rather than a column.
fn is_skip_construct(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    SKIP_FIRST_WORDS.contains(&first)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic `mysqldump --no-data`-shaped export. No real corpus
    /// table/column names or statistics — `orders` / `order_lines` /
    /// `customers` are generic e-commerce nouns chosen only to exercise the
    /// parser's DSL surface (see the required-coverage list in each test).
    const SAMPLE: &str = r#"
-- Table structure for table `customers`
--
CREATE TABLE `customers` (
  `id` int(11) NOT NULL AUTO_INCREMENT,
  `name` varchar(60) DEFAULT NULL,
  `active` tinyint(1),
  `notes` text,
  `signed_up_at` datetime,
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Table structure for table `orders`
--
CREATE TABLE `orders` (
  `id` int(11) NOT NULL AUTO_INCREMENT,
  `customer_id` int(11) NOT NULL,
  `total` decimal(10,2) NOT NULL,
  `placed_at` datetime NOT NULL,
  PRIMARY KEY (`id`),
  KEY `idx_customer` (`customer_id`),
  CONSTRAINT `fk_orders_customer` FOREIGN KEY (`customer_id`) REFERENCES `customers` (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;

-- Table structure for table `order_lines`
--
CREATE TABLE `order_lines` (
  `id` int(11) NOT NULL AUTO_INCREMENT,
  `order_id` int(11) NOT NULL,
  `sku` varchar(40) NOT NULL,
  `quantity` int(11) DEFAULT NULL,
  `broken_column
  PRIMARY KEY (`id`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
"#;

    /// (a) One [`Model`] per `CREATE TABLE`, table names verbatim — no
    /// Rails-style inflection, since a mysqldump table name IS the model
    /// name here.
    #[test]
    fn one_model_per_create_table_verbatim_name() {
        let (graph, report) = extract_schema_str(SAMPLE, "sql");

        assert_eq!(report.columns_from, "mysqldump-structure-export");
        assert_eq!(report.tables_seen, 3);
        assert_eq!(report.tables_built, 3);
        let names: Vec<&str> = graph.models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["customers", "orders", "order_lines"]);
    }

    /// (b) `field_type` + `not_null` mapping, exact per DSL form: bare
    /// `NOT NULL`, `DEFAULT NULL`, a type with no nullability suffix at
    /// all, and an `AUTO_INCREMENT` primary key.
    #[test]
    fn field_type_and_not_null_mapping_is_exact() {
        let (graph, _report) = extract_schema_str(SAMPLE, "sql");

        let customers = graph
            .models
            .iter()
            .find(|m| m.name == "customers")
            .expect("customers model");
        let by_name = |n: &str| customers.fields.iter().find(|f| f.name == n).expect(n);

        // `int NOT NULL AUTO_INCREMENT` primary key.
        let id = by_name("id");
        assert_eq!(id.field_type.as_deref(), Some("int"));
        assert_eq!(id.not_null, Some(true));

        // `varchar(60) DEFAULT NULL` — DEFAULT NULL is not NOT NULL.
        let name = by_name("name");
        assert_eq!(name.field_type.as_deref(), Some("varchar"));
        assert_eq!(name.not_null, None);

        // `tinyint(1)` — bare type, no nullability clause at all.
        let active = by_name("active");
        assert_eq!(active.field_type.as_deref(), Some("tinyint"));
        assert_eq!(active.not_null, None);

        // `text` — no nullability suffix.
        let notes = by_name("notes");
        assert_eq!(notes.field_type.as_deref(), Some("text"));
        assert_eq!(notes.not_null, None);

        // `datetime` — no nullability suffix.
        let signed_up_at = by_name("signed_up_at");
        assert_eq!(signed_up_at.field_type.as_deref(), Some("datetime"));
        assert_eq!(signed_up_at.not_null, None);

        let orders = graph
            .models
            .iter()
            .find(|m| m.name == "orders")
            .expect("orders model");
        // `decimal(10,2) NOT NULL` — internal comma inside the type args
        // must not be mistaken for the trailing-comma separator.
        let total = orders
            .fields
            .iter()
            .find(|f| f.name == "total")
            .expect("total");
        assert_eq!(total.field_type.as_deref(), Some("decimal"));
        assert_eq!(total.not_null, Some(true));
    }

    /// (c) Zero relation fabrication: every field, on every model, carries
    /// `target` / `relation_kind` / `inverse_name` all `None` — a structure
    /// dump of this shape has no FK semantics this module trusts.
    #[test]
    fn zero_relation_fabrication_on_every_field() {
        let (graph, _report) = extract_schema_str(SAMPLE, "sql");

        assert!(!graph.models.is_empty());
        for model in &graph.models {
            for field in &model.fields {
                assert_eq!(field.target, None, "{}.{}", model.name, field.name);
                assert_eq!(field.relation_kind, None, "{}.{}", model.name, field.name);
                assert_eq!(field.inverse_name, None, "{}.{}", model.name, field.name);
            }
        }
    }

    /// (d) An unparseable column line (the `order_lines` fixture's
    /// unterminated `` `broken_column `` line) lands in the report, never
    /// silently dropped.
    #[test]
    fn unparseable_column_line_is_reported_not_dropped() {
        let (_graph, report) = extract_schema_str(SAMPLE, "sql");

        assert!(
            report
                .unmatched
                .iter()
                .any(|line| line.contains("order_lines") && line.contains("broken_column")),
            "expected an unmatched entry for the unterminated column line, got: {:?}",
            report.unmatched
        );
    }

    /// (e) End-to-end address test: `extract_schema` → `expand` →
    /// `ruff_spo_address::mint_factored` mints a facet for a member field,
    /// and the `Facet::from_parts` / accessor roundtrip holds.
    #[test]
    fn schema_extracted_fields_mint_addressable_facets() {
        let (graph, _report) = extract_schema_str(SAMPLE, "sql");

        let triples = ruff_spo_triplet::expand(&graph);
        let mint = ruff_spo_address::mint_factored(&triples);

        let node = "sql:customers.name";
        let f = mint
            .facet(node)
            .unwrap_or_else(|| panic!("expected a minted facet for {node}"));
        let roundtrip = ruff_spo_address::Facet::from_parts(
            f.facet_classid(),
            f.part_of_chain(),
            f.is_a_chain(),
        );
        assert_eq!(roundtrip.to_bytes(), f.to_bytes());
    }
}
