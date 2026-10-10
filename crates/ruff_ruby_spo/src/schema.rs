//! **D-AR-3.5** — the schema stratum: physical DB columns from the Rails
//! migration DSL.
//!
//! `OpenProject` ships no `db/schema.rb` / `db/structure.sql`; the squashed
//! baseline lives in `db/migrate/tables/*.rb` — one `Tables::X <
//! Tables::Base` class per table, whose `self.table(migration)` body is a
//! plain `create_table … do |t| … end` block of `t.<type> :name, opts`
//! calls. That DSL is a fixed, enumerable vocabulary (22 distinct `t.*`
//! methods across `OpenProject`'s 99 baseline files), so a line scanner in
//! the style of [`crate::functions`] extracts it without a Ruby runtime.
//!
//! Not every Rails app squashes its history into a baseline, though —
//! Redmine (and most "classic" Rails apps) has no `db/migrate/tables/` at
//! all: the schema only ever exists as the *replay* of 300+ individual
//! `db/migrate/NNN_*.rb` / `db/migrate/<timestamp>_*.rb` files. §
//! "Two surfaces" below covers how [`extract_app_with_schema`] picks
//! between the two.
//!
//! # Why this stratum matters
//!
//! The `WorkPackage` oracle diff (op-nexgen `RESIDUAL-THREE-BUCKETS.md` §4c)
//! measured that **~90% of a hand-written Rust model struct derives from
//! the column stratum alone** (name + type + nullability), and the
//! remaining typings come from validation triples the expander already
//! ships. The class-body extraction ([`crate::extract_app_with`]) reads
//! the *method/DSL* stratum; this module supplies the missing *column*
//! stratum. Columns land as [`Field`]s (`field_type` = the DSL method
//! name verbatim, `not_null` from `null: false`), so they flow through
//! the existing `field_type` / `column_not_null` predicates with no new
//! IR shape.
//!
//! # Two surfaces (baseline squash vs classic replay)
//!
//! [`extract_app_with_schema`] sniffs the layout before parsing anything:
//!
//! - `<root>/db/migrate/tables/*.rb` exists → the **baseline** surface
//!   ([`parse_tables_dir`] / [`parse_table_source`]). Authoritative-tier
//!   for the squash itself, PLUS a bounded **post-baseline replay**
//!   ([`replay_post_baseline_migrations`]) on top of it — see "Post-baseline
//!   replay" below for exactly what that does and doesn't cover.
//! - otherwise, `<root>/db/migrate/*.rb` exists → the **classic** surface
//!   ([`parse_migrations_dir`] / [`apply_migration_source`]). **Inferred**
//!   tier, and approximate by construction: migrations are replayed in
//!   sorted-filename order (which, for both the legacy zero-padded
//!   sequence numbers and the modern 14-digit timestamp prefix, *is*
//!   migration application order) applying `create_table` + its column
//!   DSL, `change_table` (append-only — it can't be recreating a table
//!   that already exists), and `add_column`. `rename_column` /
//!   `remove_column` / `change_column` / `drop_table` are **counted, not
//!   replayed** ([`SchemaReport::unapplied_mutations`]) — correctly
//!   replaying them needs full evaluation-order tracking (a column
//!   renamed then a *new* column added under the old name; a table
//!   dropped and never recreated; …), which is out of scope for a line
//!   scanner. The count keeps the approximation honest instead of silent:
//!   a model's schema-merged fields via the classic surface are a
//!   superset of the true final schema (they can include columns that
//!   were later renamed away or removed), never a silent subset.
//!
//! # Post-baseline replay (baseline surface only)
//!
//! [`replay_post_baseline_migrations`] runs after [`parse_tables_dir`],
//! against every migration file under `<root>/db/migrate/*.rb` and
//! `<root>/modules/*/db/migrate/*.rb` (`OpenProject`'s per-module migration
//! directories), replayed in **filename-sorted** order across BOTH
//! locations together (not sorted per-directory then concatenated) —
//! Rails migration filenames are `NNNNNNNNNNNNNN_name.rb`, so sorting by
//! filename alone reproduces true application order even when a module's
//! migration lands chronologically between two core migrations. Per
//! top-level (non-block) line:
//!
//! - `add_column :table, :col, :type, opts` — appended (respecting
//!   `null: false`), unless a same-named column is already present.
//! - `rename_column :table, :old, :new` — renamed in place (position in
//!   the field list is preserved).
//! - `remove_column :table, :col` / `remove_columns :table, :a, :b` —
//!   dropped.
//! - `change_column :table, :col, :type` — the field's type is updated;
//!   best-effort, so a call whose type argument isn't a plain symbol/quoted
//!   token is silently skipped rather than guessed.
//!
//! A mutation naming a table the baseline squash didn't produce is a
//! no-op: this pass only ever refines tables [`parse_tables_dir`] already
//! matched, it never creates one. Data-only migrations (no line matching
//! one of the four forms above) contribute nothing — there's no
//! special-case needed when the line scanner simply never matches. Not
//! replayed, deliberately: `create_table` / `change_table` / `drop_table`
//! in `db/migrate/*.rb` (including `t.*` DSL lines inside a `change_table`
//! block, and multi-line calls whose table/column/type args aren't all on
//! the keyword's own line) — the squash already IS the authoritative
//! `create_table` for every table this pass can touch, and covering
//! `change_table` block bodies would need the block-tracking state
//! [`apply_migration_source`] uses for the *classic* surface, which is out
//! of scope for this additive slice (see [`SchemaReport::columns_from`]'s
//! doc for the observable consequence).
//!
//! # Scope (recorded honestly, conservation-ledger style)
//!
//! - **Baseline surface replays four mutation kinds, nothing else**: see
//!   "Post-baseline replay" above for exactly what [`SchemaReport::columns_from`]'s
//!   `"baseline+replay"` value does and doesn't cover. A corpus with no
//!   applicable post-baseline mutations is byte-identical to the
//!   pre-replay output, label included — `columns_from` only flips away
//!   from `"baseline-only"` once at least one mutation is actually
//!   applied.
//! - Join tables and other tables with no matching AR class are counted in
//!   [`SchemaReport::unmatched_tables`], never silently dropped.
//! - `t.index` / `t.foreign_key` / `t.check_constraint` /
//!   `t.exclusion_constraint` lines are constraint/index facts, not
//!   columns — skipped here (a later slice can lift them).

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use ruff_spo_triplet::{Field, Model, ModelGraph};

/// Conservation-ledger seed for the schema pass: what was seen, what
/// matched, what didn't — nothing drops silently.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemaReport {
    /// Baseline table files successfully parsed.
    pub tables_seen: usize,
    /// Tables whose inflected model name matched a model in the graph
    /// (columns merged into that model's `fields`).
    pub tables_matched: usize,
    /// Tables with no matching model (join tables, unported domains) —
    /// named, not just counted.
    pub unmatched_tables: Vec<String>,
    /// Files under `db/migrate/tables/` that could not be read or contained
    /// no recognisable `create_table` block (e.g. `base.rb`, the abstract
    /// helper) — named, not just counted.
    pub files_skipped: Vec<String>,
    /// Provenance marker: which migration surface produced the columns —
    /// `"baseline-only"` (the `Tables::X` squash, no post-baseline mutation
    /// applied), `"baseline+replay"` (the squash PLUS one or more
    /// post-baseline `add_column`/`rename_column`/`remove_column`/
    /// `change_column` statements replayed on top of it — see the module
    /// doc's "Post-baseline replay" section for exactly what that does and
    /// doesn't cover), or `"classic-migrations"` (replayed `db/migrate/*.rb`
    /// from scratch, no baseline squash to start from — approximate, see
    /// [`Self::unapplied_mutations`]).
    pub columns_from: &'static str,
    /// **Classic surface only.** Migration files under `db/migrate/`
    /// successfully read and replayed (`create_table` / `change_table` /
    /// `add_column` applied). Zero on the baseline surface.
    pub classic_migrations_scanned: usize,
    /// **Classic surface only.** Total count of `rename_column` /
    /// `remove_column` / `change_column` / `drop_table` statements seen
    /// across all scanned migrations — encountered, but deliberately NOT
    /// replayed (see the module doc's "Two surfaces" section). Zero on the
    /// baseline surface, where there is nothing to replay in the first
    /// place.
    pub unapplied_mutations: usize,
}

/// One parsed baseline table: the physical columns of `table_name`,
/// plus the Rails-inflected model name they attach to.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TableColumns {
    /// The physical table name — the file stem (`work_packages`), which is
    /// exactly what `Tables::Base.table_name` derives via
    /// `name.demodulize.underscore`.
    pub(crate) table_name: String,
    /// The Rails-conventional model name (`WorkPackage`) — `PascalCase`
    /// singular of the table name.
    pub(crate) model_name: String,
    /// The columns, in declaration order, as IR fields (`field_type` =
    /// DSL method name verbatim; `not_null` from `null: false`).
    pub(crate) fields: Vec<Field>,
}

/// The `t.<method>` names that declare a typed column directly. The DSL
/// method name doubles as the emitted `field_type` token (surface label —
/// consumers own the SQL/SurrealQL mapping).
const COLUMN_TYPES: &[&str] = &[
    "string",
    "text",
    "integer",
    "bigint",
    "boolean",
    "datetime",
    "date",
    "float",
    "decimal",
    "jsonb",
    "json",
    "uuid",
    "interval",
    "tsvector",
    "tstzrange",
    "binary",
    "timestamp",
];

/// Extract a Rails app **including the schema stratum**: everything
/// [`crate::extract_app_with`] harvests, plus the baseline DB columns
/// merged into each model's `fields`, plus a [`SchemaReport`] ledger.
///
/// Column fields are appended to the matching model's `fields` (matched by
/// Rails inflection of the table name; existing same-name fields, if any
/// future pass creates them, are not duplicated). Tables with no matching
/// model are recorded in the report — the join-table population is real
/// and expected (`changesets_work_packages` et al. have no AR class).
///
/// After the column merge, a **compute-linkage pass** (`link_computed_fields`)
/// runs over every model: a `def compute_<x>` whose class ALSO has a
/// (schema-merged) field named `<x>` gets `field.emitted_by = Some("compute_<x>")`
/// — the Rails-side equivalent of Odoo's declared `compute=`. This only ever
/// runs here (the schema-aware path), because the model-only stratum
/// (`crate::extract_fields`) never populates `fields` at all — the pass
/// would be a no-op there. It never synthesizes a `Field` from a method name
/// alone: linkage requires the field to already exist.
///
/// # Layout sniffing (baseline vs classic)
///
/// `<root>/db/migrate/tables/` is checked first: if it contains any `.rb`
/// file, this is the `OpenProject`-style squashed baseline and the
/// `parse_tables_dir` path runs, followed by
/// `replay_post_baseline_migrations` (module doc: "Post-baseline
/// replay"). Otherwise, if `<root>/db/migrate/` itself contains any `.rb`
/// file, this is a classic Rails app (Redmine and similar — no baseline
/// squash, only the full migration history) and `parse_migrations_dir`
/// runs instead — untouched by this pass. Neither directory existing
/// leaves [`SchemaReport::tables_seen`] at zero, same as before the classic
/// fallback was added.
#[must_use]
pub fn extract_app_with_schema(source_tree: &Path, namespace: &str) -> (ModelGraph, SchemaReport) {
    let mut graph = crate::extract_app_with(source_tree, namespace);
    let use_classic_migrations = !dir_has_rb_files(&source_tree.join("db/migrate/tables"))
        && dir_has_rb_files(&source_tree.join("db/migrate"));
    let mut report = SchemaReport {
        columns_from: if use_classic_migrations {
            "classic-migrations"
        } else {
            "baseline-only"
        },
        ..SchemaReport::default()
    };

    let mut tables = if use_classic_migrations {
        parse_migrations_dir(source_tree, &mut report)
    } else {
        parse_tables_dir(source_tree, &mut report)
    };
    if !use_classic_migrations && replay_post_baseline_migrations(source_tree, &mut tables) > 0 {
        report.columns_from = "baseline+replay";
    }
    for table in tables {
        report.tables_seen += 1;
        if let Some(model) = graph.models.iter_mut().find(|m| m.name == table.model_name) {
            report.tables_matched += 1;
            for field in table.fields {
                if !model.fields.iter().any(|f| f.name == field.name) {
                    model.fields.push(field);
                }
            }
        } else {
            report.unmatched_tables.push(table.table_name);
        }
    }
    report.unmatched_tables.sort();
    report.files_skipped.sort();

    for model in &mut graph.models {
        link_computed_fields(model);
    }

    (graph, report)
}

/// The set of Rails-inflected model names backed by a real DB table — the
/// roster [`crate::menu_regions`]'s identity-binding arm cross-checks a
/// `controller → model` derived token against before emitting
/// `surfaces_concept` at [`ruff_spo_triplet::Provenance::OpenProjectExtracted`].
///
/// Lightweight: parses only the migration/table DSL via the same
/// baseline-vs-classic layout sniffing [`extract_app_with_schema`] uses
/// (`db/migrate/tables/*.rb` squash if present, else `db/migrate/*.rb`
/// classic replay) — no class-body extraction, no [`ModelGraph`] built. The
/// roster IS the honesty mechanism: a derived token that doesn't name a real
/// table-backed model resolves to `derived_unmatched`, never a fabricated
/// `surfaces_concept`.
#[must_use]
pub(crate) fn model_roster(source_tree: &Path) -> HashSet<String> {
    let use_classic_migrations = !dir_has_rb_files(&source_tree.join("db/migrate/tables"))
        && dir_has_rb_files(&source_tree.join("db/migrate"));
    let mut report = SchemaReport::default();
    let tables = if use_classic_migrations {
        parse_migrations_dir(source_tree, &mut report)
    } else {
        parse_tables_dir(source_tree, &mut report)
    };
    tables.into_iter().map(|t| t.model_name).collect()
}

/// **D-AR-3.5 compute linkage.** For each `def compute_<x>` in `model.functions`,
/// if `model.fields` already has a field named `<x>` (schema-merged or
/// otherwise) with no `emitted_by` yet, set `field.emitted_by =
/// Some("compute_<x>")`.
///
/// This is grounded on BOTH sides — the column exists (schema stratum) AND
/// the def exists (method-name stratum, `class::extract_functions_from_body`)
/// — so it never synthesizes a [`Field`] from a method name alone: a
/// `compute_<x>` def with no matching `<x>` field links nothing and creates
/// nothing (the guardrail, pinned by
/// [`tests::link_computed_fields_does_not_synthesize_a_field_for_an_unmatched_compute_def`]).
/// An existing `emitted_by` (set by some future richer pass) is never
/// overwritten.
pub(crate) fn link_computed_fields(model: &mut Model) {
    let compute_targets: Vec<&str> = model
        .functions
        .iter()
        .filter_map(|f| f.name.strip_prefix("compute_"))
        .collect();
    for field in &mut model.fields {
        if field.emitted_by.is_none() && compute_targets.contains(&field.name.as_str()) {
            field.emitted_by = Some(format!("compute_{}", field.name));
        }
    }
}

/// `true` when `dir` exists and contains at least one `.rb` file
/// (non-recursive). The layout-sniffing probe [`extract_app_with_schema`]
/// uses to pick baseline-squash vs classic-replay.
fn dir_has_rb_files(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|e| e.path().extension().and_then(|e| e.to_str()) == Some("rb"))
}

/// Parse every baseline table file under `<root>/db/migrate/tables/`.
/// Deterministic: files are sorted before parsing (same discipline as
/// [`crate::parse`]'s walk). Unreadable / unrecognisable files land in the
/// report's `files_skipped`, not on the floor.
pub(crate) fn parse_tables_dir(source_tree: &Path, report: &mut SchemaReport) -> Vec<TableColumns> {
    let dir = source_tree.join("db/migrate/tables");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rb"))
        .collect();
    files.sort();

    let mut tables = Vec::with_capacity(files.len());
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let Ok(src) = fs::read_to_string(&path) else {
            report.files_skipped.push(stem);
            continue;
        };
        match parse_table_source(&stem, &src) {
            Some(table) => tables.push(table),
            None => report.files_skipped.push(stem),
        }
    }
    tables
}

/// Parse one baseline table file's source. `None` when the file has no
/// `create_table` block (e.g. `base.rb`, the abstract helper class).
pub(crate) fn parse_table_source(table_name: &str, src: &str) -> Option<TableColumns> {
    let mut fields: Vec<Field> = Vec::new();
    let mut in_block = false;
    let mut saw_create_table = false;

    for raw in src.lines() {
        let line = raw.trim();
        if !in_block {
            if line.starts_with("create_table") || line.starts_with("create_unlogged_table") {
                saw_create_table = true;
                in_block = true;
                // Implicit primary key unless the create_table call opts out.
                if !line.contains("id: false") {
                    fields.push(column_field("id", "bigint", true));
                }
            }
            continue;
        }
        if line == "end" {
            // First `end` at block depth closes the `do |t|` block. The
            // baseline files nest nothing deeper inside it.
            break;
        }
        let Some(rest) = line.strip_prefix("t.") else {
            continue;
        };
        let (method, args) = split_method_args(rest);
        fields.extend(fields_from_column_dsl(method, args));
    }

    if !saw_create_table {
        return None;
    }
    Some(TableColumns {
        table_name: table_name.to_string(),
        model_name: model_name_for_table(table_name),
        fields,
    })
}

// ────────────────── baseline + post-baseline replay ──────────────────

/// **Post-baseline replay.** After [`parse_tables_dir`] establishes each
/// table's baseline columns, replay every migration file under
/// `<root>/db/migrate/*.rb` and `<root>/modules/*/db/migrate/*.rb` — in
/// filename-sorted (timestamp) order across both locations together — on
/// top of them. See the module doc's "Post-baseline replay" section for
/// exactly what's applied and what isn't. Returns the number of mutations
/// actually applied (0 when there is nothing to replay, or nothing
/// replayable was found among what there is), which the caller uses to
/// decide whether [`SchemaReport::columns_from`] should flip to
/// `"baseline+replay"`.
pub(crate) fn replay_post_baseline_migrations(
    source_tree: &Path,
    tables: &mut [TableColumns],
) -> usize {
    let mut files = collect_post_baseline_migration_files(source_tree);
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));

    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for (i, table) in tables.iter().enumerate() {
        index.insert(table.table_name.clone(), i);
    }

    let mut applied = 0;
    for path in files {
        if let Ok(src) = fs::read_to_string(&path) {
            applied += replay_migration_source(&src, tables, &index);
        }
    }
    applied
}

/// Every `.rb` file directly under `<root>/db/migrate/`, plus every `.rb`
/// file directly under each `<root>/modules/*/db/migrate/` (one per
/// module). Unsorted — callers sort by filename for cross-directory
/// timestamp order (see [`replay_post_baseline_migrations`]).
fn collect_post_baseline_migration_files(source_tree: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    push_rb_files(&source_tree.join("db/migrate"), &mut files);
    if let Ok(modules) = fs::read_dir(source_tree.join("modules")) {
        for module in modules.flatten() {
            push_rb_files(&module.path().join("db/migrate"), &mut files);
        }
    }
    files
}

/// Push every direct (non-recursive) `.rb` file under `dir` onto `out`. A
/// missing/unreadable `dir` contributes nothing — same tolerant-of-absence
/// discipline as [`dir_has_rb_files`].
fn push_rb_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    out.extend(
        entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("rb")),
    );
}

/// Replay one migration file's `add_column` / `rename_column` /
/// `remove_column` / `remove_columns` / `change_column` statements against
/// `tables` (keyed by table name via `index`). Top-level lines only — same
/// single-statement-per-line discipline as [`apply_migration_source`]. A
/// table not in `index` is left untouched (module doc: this pass never
/// invents one). Returns the count of mutations actually applied: a line
/// that matches one of the four forms but names an unknown table, or (for
/// `rename_column`/`remove_column(s)`/`change_column`) an unknown column,
/// applies nothing and isn't counted.
/// Heuristic Ruby block-opener detector for the replay depth tracker: a
/// leading block keyword (`def`/`if`/`unless`/`while`/`until`/`case`/
/// `begin`/`for`/`class`/`module`) or a trailing `do` / `do |...|`. Modifier
/// forms (`add_column … if cond`) don't match — the keyword must lead. Paired
/// with an `== "end"` close check; single-line `def … end` (absent from
/// real migrations) is out of scope.
fn is_ruby_block_opener(line: &str) -> bool {
    if line.ends_with(" do") || line.contains(" do ") || line.contains(" do|") {
        return true;
    }
    let head = line
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    matches!(
        head,
        "def" | "if" | "unless" | "while" | "until" | "case" | "begin" | "for" | "class" | "module"
    )
}

fn replay_migration_source(
    src: &str,
    tables: &mut [TableColumns],
    index: &BTreeMap<String, usize>,
) -> usize {
    let mut applied = 0;
    // Skip `def down` rollback bodies: `def up`/`def change` describe the
    // schema AFTER the migration, `def down` reverses it. Replaying the down
    // half would undo valid columns (add-then-remove). Track Ruby block depth
    // and suppress mutations while inside the `def down` method (from its
    // opening `def` line to the `end` that returns to the enclosing depth).
    let mut depth: i32 = 0;
    let mut down_from: Option<i32> = None;
    for raw in src.lines() {
        let line = raw.trim();

        if down_from.is_none()
            && (line == "def down"
                || line.starts_with("def down(")
                || line.starts_with("def down "))
        {
            down_from = Some(depth);
        }
        let in_rollback = down_from.is_some();

        let opens = is_ruby_block_opener(line);
        let closes = line == "end" || line.starts_with("end ") || line.starts_with("end;");
        if opens {
            depth += 1;
        }
        if closes {
            depth -= 1;
            if let Some(d) = down_from
                && depth <= d
            {
                down_from = None;
            }
        }

        if in_rollback {
            continue;
        }

        if let Some((table, field)) = parse_add_column(line) {
            if let Some(&i) = index.get(&table) {
                let before = tables[i].fields.len();
                push_field_if_absent(&mut tables[i].fields, field);
                if tables[i].fields.len() != before {
                    applied += 1;
                }
            }
            continue;
        }
        if let Some((table, old, new)) = parse_rename_column(line) {
            if let Some(&i) = index.get(&table) {
                if let Some(f) = tables[i].fields.iter_mut().find(|f| f.name == old) {
                    f.name = new;
                    applied += 1;
                }
            }
            continue;
        }
        if let Some((table, names)) = parse_remove_columns(line) {
            if let Some(&i) = index.get(&table) {
                let before = tables[i].fields.len();
                tables[i].fields.retain(|f| !names.contains(&f.name));
                if tables[i].fields.len() != before {
                    applied += 1;
                }
            }
            continue;
        }
        if let Some((table, name, ty)) = parse_change_column(line) {
            if let Some(&i) = index.get(&table) {
                if let Some(f) = tables[i].fields.iter_mut().find(|f| f.name == name) {
                    f.field_type = Some(ty);
                    applied += 1;
                }
            }
        }
    }
    applied
}

/// An optional pair of enclosing call parentheses around a keyword's
/// argument tail: `"(:a, :b)"` → `":a, :b"`; anything without BOTH a
/// leading `(` and a trailing `)` is returned unchanged (already-bare
/// arguments, e.g. `" :a, :b"`, or an unbalanced fragment we shouldn't
/// guess about). Rails migrations mix both call styles freely — real
/// example from the `OpenProject` corpus: `change_column(:documents,
/// :title, :string, limit:)` alongside bare `add_column :t, :c, :type`
/// everywhere else — so the three parsers below tolerate either.
fn strip_call_parens(rest: &str) -> &str {
    let trimmed = rest.trim();
    trimmed
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(rest)
}

/// `rename_column :table, :old, :new` (or quoted-string / parenthesized
/// forms) → the table name and the (old, new) column-name pair. `None` for
/// anything else.
fn parse_rename_column(line: &str) -> Option<(String, String, String)> {
    let rest = line.strip_prefix("rename_column")?;
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rest = strip_call_parens(rest.trim_start());
    let mut parts = rest.split(',').map(str::trim);
    let table = parts.next().and_then(name_token)?.to_string();
    let old = parts.next().and_then(name_token)?.to_string();
    let new = parts.next().and_then(name_token)?.to_string();
    Some((table, old, new))
}

/// `remove_column :table, :name` / `remove_columns :table, :a, :b, …` (or
/// quoted-string / parenthesized forms) → the table name and the column
/// name(s) to drop. `remove_columns` (plural, variadic) is checked first —
/// `remove_column` is a textual prefix of it, the same kind of collision
/// the `_default`/`_null` suffixes create for `change_column` (see
/// [`is_mutation_call`]).
fn parse_remove_columns(line: &str) -> Option<(String, Vec<String>)> {
    let rest = line
        .strip_prefix("remove_columns")
        .or_else(|| line.strip_prefix("remove_column"))?;
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rest = strip_call_parens(rest.trim_start());
    let mut parts = rest.split(',').map(str::trim);
    let table = parts.next().and_then(name_token)?.to_string();
    let names: Vec<String> = parts.filter_map(name_token).map(str::to_string).collect();
    if names.is_empty() {
        return None;
    }
    Some((table, names))
}

/// `change_column :table, :name, :type, opts` (or quoted-string /
/// parenthesized forms) → the table name, column name, and new type token.
/// Best-effort: `None` when the type argument isn't a plain symbol/quoted
/// name token (e.g. a computed/dynamic expression) — skipped rather than
/// guessed, per the module's "Scope" discipline.
fn parse_change_column(line: &str) -> Option<(String, String, String)> {
    let rest = line.strip_prefix("change_column")?;
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rest = strip_call_parens(rest.trim_start());
    let mut parts = rest.split(',').map(str::trim);
    let table = parts.next().and_then(name_token)?.to_string();
    let name = parts.next().and_then(name_token)?.to_string();
    let ty = parts.next().and_then(name_token)?.to_string();
    Some((table, name, ty))
}

// ────────────────── classic `db/migrate/*.rb` replay ──────────────────

/// Parse every classic Rails migration file under `<root>/db/migrate/` (the
/// "no baseline squash" layout — Redmine and similar corpora). Files are
/// processed in **sorted filename order** — for classic Rails migrations,
/// filename order (the legacy zero-padded sequence number, e.g.
/// `001_setup.rb`, or the modern 14-digit timestamp prefix) *is* migration
/// application order, so replaying files in that order reproduces the
/// schema evolution `db/schema.rb` would otherwise have recorded, without
/// ever needing `schema.rb` to exist.
///
/// Returns one [`TableColumns`] per table name ever mentioned by a
/// `create_table` / `change_table` / `add_column`, sorted by table name for
/// deterministic output (migration-file order still governs each table's
/// *own* field order — see [`apply_migration_source`]). Approximate /
/// **Inferred**-tier by design: see the module doc's "Two surfaces"
/// section for what is and isn't replayed.
pub(crate) fn parse_migrations_dir(
    source_tree: &Path,
    report: &mut SchemaReport,
) -> Vec<TableColumns> {
    let dir = source_tree.join("db/migrate");
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().and_then(|e| e.to_str()) == Some("rb"))
        .collect();
    files.sort();

    let mut tables: BTreeMap<String, Vec<Field>> = BTreeMap::new();
    for path in files {
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let Ok(src) = fs::read_to_string(&path) else {
            report.files_skipped.push(stem);
            continue;
        };
        report.classic_migrations_scanned += 1;
        apply_migration_source(&src, &mut tables, report);
    }

    tables
        .into_iter()
        .map(|(table_name, fields)| TableColumns {
            model_name: model_name_for_table(&table_name),
            table_name,
            fields,
        })
        .collect()
}

/// Replay one migration file's source against the running `tables` state.
///
/// Recognises, per top-level (non-block) line:
/// - `create_table "name"` / `create_table :name` (options ignored, except
///   `id: false` / `:id => false` suppressing the implicit PK) — opens a
///   column block AND **replaces** that table's column list (an idempotent
///   re-create further down the migration history), same as a fresh table.
/// - `change_table :name` — opens the SAME kind of column block, but never
///   clears: a `change_table` can only target a table that already exists,
///   so its `t.*` lines are batch `add_column`s (append-if-absent), not a
///   redefinition. (Redmine's `20180913072918_add_verify_peer_to_auth_sources.rb`
///   is exactly this shape.)
/// - `add_column :table, :col, :type` (or quoted-string table/col) —
///   appends the column if a same-named one isn't already present.
/// - `rename_column` / `remove_column` / `change_column` / `drop_table` —
///   counted in [`SchemaReport::unapplied_mutations`], never replayed (see
///   the module doc).
///
/// A column block closes at the first bare `end` line — the same
/// single-nesting-depth assumption [`parse_table_source`] documents holds
/// here too (verified against the full Redmine corpus: no `create_table` /
/// `change_table` body nests another `do |x| … end`).
fn apply_migration_source(
    src: &str,
    tables: &mut BTreeMap<String, Vec<Field>>,
    report: &mut SchemaReport,
) {
    let mut current_table: Option<String> = None;

    for raw in src.lines() {
        let line = raw.trim();

        if let Some(table_name) = &current_table {
            if line == "end" {
                current_table = None;
                continue;
            }
            let Some(rest) = line.strip_prefix("t.") else {
                continue;
            };
            let (method, args) = split_method_args(rest);
            let entry = tables.entry(table_name.clone()).or_default();
            for field in fields_from_column_dsl(method, args) {
                push_field_if_absent(entry, field);
            }
            continue;
        }

        if let Some((name, id_false)) = parse_create_table_opener(line) {
            tables.insert(
                name.clone(),
                if id_false {
                    Vec::new()
                } else {
                    vec![column_field("id", "bigint", true)]
                },
            );
            current_table = Some(name);
            continue;
        }
        if let Some(name) = parse_change_table_opener(line) {
            tables.entry(name.clone()).or_default();
            current_table = Some(name);
            continue;
        }

        if let Some((table, field)) = parse_add_column(line) {
            push_field_if_absent(tables.entry(table).or_default(), field);
            continue;
        }

        if [
            "rename_column",
            "remove_column",
            "change_column",
            "drop_table",
        ]
        .into_iter()
        .any(|keyword| is_mutation_call(line, keyword))
        {
            report.unapplied_mutations += 1;
        }
    }
}

/// Append `field` to `fields` unless a same-named column is already
/// present — the "appends if absent" discipline `add_column` /
/// `change_table` need (a migration re-adding a column it already added,
/// or a column the baseline-shape scan would otherwise duplicate).
fn push_field_if_absent(fields: &mut Vec<Field>, field: Field) {
    if !fields.iter().any(|f| f.name == field.name) {
        fields.push(field);
    }
}

/// Classic-migration `create_table` opener: `create_table "name", opts do
/// |t|` or `create_table :name, opts do |t|`. Returns the table name and
/// whether `id: false` / `:id => false` suppresses the implicit PK. `None`
/// for any other line — including the baseline DSL's `create_table
/// migration do |t|` form (`migration` is a bare local, not a table-name
/// literal), which never appears in classic migrations.
fn parse_create_table_opener(line: &str) -> Option<(String, bool)> {
    let rest = line.strip_prefix("create_table")?;
    // Guard against `create_tables`/`create_table_foo` identifiers: the
    // next byte must not continue an identifier.
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    let rest = rest.trim_start();
    let name = first_positional_name(rest)?;
    let id_false = rest.contains("id: false") || rest.contains(":id => false");
    Some((name.to_string(), id_false))
}

/// Classic-migration `change_table` opener: `change_table :name do |t|`.
/// Same shape as [`parse_create_table_opener`] minus the implicit-PK /
/// `id: false` bookkeeping (a `change_table` never (re)creates the table).
fn parse_change_table_opener(line: &str) -> Option<String> {
    let rest = line.strip_prefix("change_table")?;
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    first_positional_name(rest.trim_start()).map(str::to_string)
}

/// The first positional argument of a `create_table`/`change_table` call
/// tail (everything after the keyword), as a name token — stops at the
/// first `,` or whitespace, whichever comes first, so both
/// `"name", opts do |t|` and `:name do |t|` (no comma before `do`) extract
/// just the name.
fn first_positional_name(rest: &str) -> Option<&str> {
    let end = rest.find([',', ' ']).unwrap_or(rest.len());
    name_token(&rest[..end])
}

/// `add_column :table, :col, :type, opts` (or quoted-string table/col) → the
/// table name and the new [`Field`]. `None` for anything else (including a
/// malformed call missing the type, which the DSL requires).
fn parse_add_column(line: &str) -> Option<(String, Field)> {
    let rest = line.strip_prefix("add_column")?;
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    // Normalise the parenthesized call form `add_column(:t, :c, :type)` the
    // same way the rename/remove/change parsers do — otherwise the leading
    // `(` rides into the first token and the whole mutation is dropped.
    let rest = strip_call_parens(rest.trim_start());
    let mut parts = rest.split(',').map(str::trim);
    let table = parts.next().and_then(name_token)?.to_string();
    let name = parts.next().and_then(name_token)?;
    let ty = parts.next().and_then(name_token)?;
    let not_null = parse_not_null(rest);
    Some((table, column_field(name, ty, not_null)))
}

/// `true` when `line` is a top-level classic-migration call to `keyword`
/// (`rename_column` / `remove_column` / `change_column` / `drop_table`) —
/// the keyword followed by whitespace or `(`, so e.g.
/// `change_column_default` / `change_column_null` (distinct DSL calls)
/// never false-match `change_column`.
fn is_mutation_call(line: &str, keyword: &str) -> bool {
    line.strip_prefix(keyword)
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace() || c == '('))
}

/// Split a `t.<method> <args>` line's remainder (post `t.` strip) into the
/// method name and its trimmed argument tail (`""` for a bare method like
/// `t.timestamps`).
fn split_method_args(rest: &str) -> (&str, &str) {
    match rest.split_once(char::is_whitespace) {
        Some((m, a)) => (m, a.trim()),
        None => (rest, ""),
    }
}

/// Given one `t.<method> <args>` line's parts, produce the column
/// [`Field`]s it declares (0, 1, or 2 — `references`/`belongs_to` can
/// yield an `_id` + `_type` pair, `timestamps` yields the
/// `created_at`/`updated_at` pair).
///
/// Shared by both migration surfaces: the `Tables::X` baseline DSL
/// ([`parse_table_source`]) and classic `db/migrate/*.rb` migrations
/// ([`apply_migration_source`]). Column/type tokens accept either a Ruby
/// symbol (`:name`) or a quoted string (`"name"` / `'name'`) via
/// [`name_token`] — classic migrations use both across Rails' history
/// (`t.column "container_id", :integer, …` is the dominant classic form);
/// the baseline DSL only ever used symbols, so this is a strict superset
/// and changes nothing for the baseline surface.
fn fields_from_column_dsl(method: &str, args: &str) -> Vec<Field> {
    match method {
        // Constraint / index facts, not columns.
        "index" | "foreign_key" | "check_constraint" | "exclusion_constraint" => Vec::new(),
        // `t.timestamps precision: nil, null: true` → the pair.
        "timestamps" => {
            let not_null = parse_not_null(args);
            vec![
                column_field("created_at", "datetime", not_null),
                column_field("updated_at", "datetime", not_null),
            ]
        }
        // `t.references :x, null: false, polymorphic: true` (alias
        // `belongs_to`) → `x_id` bigint, plus `x_type` string when
        // polymorphic.
        "references" | "belongs_to" => {
            let mut out = Vec::new();
            if let Some(name) = first_name_arg(args) {
                let not_null = parse_not_null(args);
                out.push(column_field(&format!("{name}_id"), "bigint", not_null));
                if args.contains("polymorphic: true") {
                    out.push(column_field(&format!("{name}_type"), "string", not_null));
                }
            }
            out
        }
        // `t.column :name, :type, opts` / `t.column "name", :type, opts` —
        // the explicit form.
        "column" => {
            let mut parts = args.split(',').map(str::trim);
            let name = parts.next().and_then(name_token);
            let ty = parts.next().and_then(name_token);
            match (name, ty) {
                (Some(name), Some(ty)) => vec![column_field(name, ty, parse_not_null(args))],
                _ => Vec::new(),
            }
        }
        // `t.<type> :name, opts` / `t.<type> "name", opts` — the direct
        // typed forms.
        m if COLUMN_TYPES.contains(&m) => first_name_arg(args)
            .map(|name| vec![column_field(name, m, parse_not_null(args))])
            .unwrap_or_default(),
        // Unknown t.* method: not a column declaration we recognise.
        // The closed COLUMN_TYPES list + this arm make additions an
        // explicit act (same discipline as the Predicate count-lock).
        _ => Vec::new(),
    }
}

/// Build one column [`Field`]: `field_type` carries the DSL method name
/// verbatim; `not_null` only when the DSL says `null: false`.
fn column_field(name: &str, dsl_type: &str, not_null: bool) -> Field {
    Field {
        name: name.to_string(),
        field_type: Some(dsl_type.to_string()),
        not_null: if not_null { Some(true) } else { None },
        ..Field::default()
    }
}

/// `null: false` (the modern keyword-argument spelling) or `:null => false`
/// (the hash-rocket spelling classic migrations use — Redmine's
/// `db/migrate/001_setup.rb` predates Ruby 1.9 hash-literal shorthand)
/// anywhere in the arg list → NOT NULL. Rails' default for columns is
/// nullable, so absence (or explicit `null: true` / `:null => true`) is
/// `false`.
fn parse_not_null(args: &str) -> bool {
    args.contains("null: false") || args.contains(":null => false")
}

/// The first name argument — a `:symbol` or a quoted string — e.g.
/// `":subject, default: …"` → `subject`, or `"\"subject\", default: …"` →
/// `subject`.
fn first_name_arg(args: &str) -> Option<&str> {
    args.split(',').next().and_then(name_token)
}

/// One column/type token: a Ruby symbol (`:name`) or a quoted string
/// (`"name"` / `'name'`). `":name"` → `name`; `"\"name\""` → `name`
/// (surrounding whitespace tolerated on both forms).
fn name_token(part: &str) -> Option<&str> {
    let part = part.trim();
    if let Some(sym) = part.strip_prefix(':') {
        return Some(sym.trim_end());
    }
    for quote in ['"', '\''] {
        if let Some(rest) = part.strip_prefix(quote) {
            return rest.find(quote).map(|end| &rest[..end]);
        }
    }
    None
}

/// Rails inflection, table → model: `snake_case` plural → `PascalCase`
/// singular (`work_packages` → `WorkPackage`). Only the last segment is
/// singularised. The rule chain covers the `OpenProject` baseline corpus;
/// genuinely irregular names belong in `IRREGULAR`, and a miss lands the
/// table in `unmatched_tables` (visible), never on a wrong model.
pub(crate) fn model_name_for_table(table: &str) -> String {
    /// Table names whose singular is not rule-derivable.
    const IRREGULAR: &[(&str, &str)] = &[
        ("news", "news"),
        ("meeting_agenda_item_series", "meeting_agenda_item_series"),
    ];

    let segments: Vec<&str> = table.split('_').collect();
    let mut out = String::new();
    let last = segments.len().saturating_sub(1);
    for (i, seg) in segments.iter().enumerate() {
        let word = if i == last {
            singularize(seg, table, IRREGULAR)
        } else {
            (*seg).to_string()
        };
        let mut chars = word.chars();
        if let Some(first) = chars.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// Singularise one `snake_case` segment (the table's final word).
///
/// `pub(crate)` (E4, the routes.rb harvest arm): the ALGORITHM is shared
/// crate-wide — `crate::routes::singularize_local` calls this with its own
/// routes-local `IRREGULAR` table — while the constant `IRREGULAR` table
/// above stays fn-local to this module (routes.rs duplicates the pairs it
/// needs rather than importing this module's private constant).
pub(crate) fn singularize(seg: &str, full_table: &str, irregular: &[(&str, &str)]) -> String {
    if let Some((_, singular)) = irregular.iter().find(|(t, _)| *t == full_table) {
        return (*singular).to_string();
    }
    if let Some(stem) = seg.strip_suffix("ies") {
        return format!("{stem}y");
    }
    for es_suffix in ["sses", "shes", "ches", "xes", "zes", "uses"] {
        if seg.ends_with(es_suffix) {
            return seg[..seg.len() - 2].to_string();
        }
    }
    seg.strip_suffix('s').unwrap_or(seg).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
class Tables::WorkPackages < Tables::Base
  def self.table(migration)
    create_table migration do |t|
      t.references :type, null: false, index: true, foreign_key: { on_delete: :cascade }
      t.string :subject, default: "", null: false
      t.text :description
      t.integer :done_ratio, default: nil, null: true
      t.timestamps precision: nil, null: true, index: true
      t.belongs_to :responsible
      t.boolean :schedule_manually, default: true, null: false
      t.references :reactable, polymorphic: true, null: false
      t.column :builtin, :boolean, default: false, null: false

      t.index %i[project_id updated_at]
      t.check_constraint "due_date >= start_date", name: "x"
    end
  end
end
"#;

    #[test]
    fn parses_the_dsl_forms() {
        let t = parse_table_source("work_packages", SAMPLE).expect("create_table block");
        assert_eq!(t.model_name, "WorkPackage");
        let names: Vec<&str> = t.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "id",
                "type_id",
                "subject",
                "description",
                "done_ratio",
                "created_at",
                "updated_at",
                "responsible_id",
                "schedule_manually",
                "reactable_id",
                "reactable_type",
                "builtin",
            ]
        );
        let by_name = |n: &str| t.fields.iter().find(|f| f.name == n).unwrap();
        // Implicit PK.
        assert_eq!(by_name("id").field_type.as_deref(), Some("bigint"));
        assert_eq!(by_name("id").not_null, Some(true));
        // references → _id bigint, null: false honoured.
        assert_eq!(by_name("type_id").field_type.as_deref(), Some("bigint"));
        assert_eq!(by_name("type_id").not_null, Some(true));
        // Plain nullable column: no positive fact.
        assert_eq!(by_name("description").field_type.as_deref(), Some("text"));
        assert_eq!(by_name("description").not_null, None);
        // Explicit null: true stays absent (nullable is the default).
        assert_eq!(by_name("done_ratio").not_null, None);
        // timestamps pair, honouring null: true.
        assert_eq!(
            by_name("created_at").field_type.as_deref(),
            Some("datetime")
        );
        assert_eq!(by_name("created_at").not_null, None);
        // belongs_to alias.
        assert_eq!(
            by_name("responsible_id").field_type.as_deref(),
            Some("bigint")
        );
        // Polymorphic pair — the PolyRef substrate declaring itself.
        assert_eq!(by_name("reactable_id").not_null, Some(true));
        assert_eq!(
            by_name("reactable_type").field_type.as_deref(),
            Some("string")
        );
        // t.column explicit form.
        assert_eq!(by_name("builtin").field_type.as_deref(), Some("boolean"));
        assert_eq!(by_name("builtin").not_null, Some(true));
    }

    #[test]
    fn id_false_suppresses_the_implicit_pk() {
        let src = "create_table migration, id: false do |t|\n  t.bigint :a_id\nend\n";
        let t = parse_table_source("a_b_joins", src).expect("block");
        assert_eq!(t.fields.len(), 1);
        assert_eq!(t.fields[0].name, "a_id");
    }

    #[test]
    fn base_helper_file_is_not_a_table() {
        assert!(parse_table_source("base", "class Tables::Base\nend\n").is_none());
    }

    // ────────────────── classic `db/migrate/*.rb` fallback (Redmine-style) ──────────────────

    use std::path::PathBuf;

    fn write_migration(root: &Path, rel: &str, content: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, content).unwrap();
    }

    fn scratch_dir(case: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "ruff_ruby_spo_schema_classic_{}_{case}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    /// `create_table` with both `t.column "name", :type` (the dominant
    /// classic form) and the direct `t.<type> :name` / `t.<type> "name"`
    /// forms in the same block.
    #[test]
    fn classic_create_table_parses_column_and_typed_forms() {
        let root = scratch_dir("dsl-forms");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.column "name", :string, :default => "", :null => false
      t.integer "count"
      t.string :label
      t.text "notes"
    end
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        assert_eq!(tables.len(), 1);
        let widgets = &tables[0];
        assert_eq!(widgets.table_name, "widgets");
        assert_eq!(widgets.model_name, "Widget");
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "count", "label", "notes"]);

        let by_name = |n: &str| widgets.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("name").field_type.as_deref(), Some("string"));
        assert_eq!(by_name("name").not_null, Some(true));
        assert_eq!(by_name("count").field_type.as_deref(), Some("integer"));
        assert_eq!(by_name("label").field_type.as_deref(), Some("string"));
        assert_eq!(by_name("notes").field_type.as_deref(), Some("text"));
        assert_eq!(report.classic_migrations_scanned, 1);
        assert_eq!(report.unapplied_mutations, 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// `t.timestamps` adds the `created_at`/`updated_at` pair, honouring
    /// `null: false`.
    #[test]
    fn classic_t_timestamps_adds_created_and_updated_at() {
        let root = scratch_dir("timestamps");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table :widgets do |t|
      t.string :name
      t.timestamps null: false
    end
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "created_at", "updated_at"]);
        let by_name = |n: &str| widgets.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("created_at").not_null, Some(true));
        assert_eq!(by_name("updated_at").not_null, Some(true));

        let _ = fs::remove_dir_all(&root);
    }

    /// `t.references` adds the `_id` column (bigint), honouring `null: false`.
    #[test]
    fn classic_t_references_adds_id_column() {
        let root = scratch_dir("references");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table :widgets do |t|
      t.references :project, null: false
    end
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "project_id"]);
        let by_name = |n: &str| widgets.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("project_id").field_type.as_deref(), Some("bigint"));
        assert_eq!(by_name("project_id").not_null, Some(true));

        let _ = fs::remove_dir_all(&root);
    }

    /// A later `add_column` IS applied — appended to the table's running
    /// column list.
    #[test]
    fn classic_add_column_is_applied() {
        let root = scratch_dir("add-column");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.column "name", :string
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/002_add_price.rb",
            r#"
class AddPrice < ActiveRecord::Migration[4.2]
  def self.up
    add_column :widgets, :price, :integer, :default => 0, :null => false
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "price"]);
        let by_name = |n: &str| widgets.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("price").field_type.as_deref(), Some("integer"));
        assert_eq!(by_name("price").not_null, Some(true));
        assert_eq!(report.classic_migrations_scanned, 2);
        assert_eq!(report.unapplied_mutations, 0);

        let _ = fs::remove_dir_all(&root);
    }

    /// `rename_column` is COUNTED, never applied — the old name survives
    /// untouched and the report's ledger increments.
    #[test]
    fn classic_rename_column_is_counted_not_applied() {
        let root = scratch_dir("rename-column");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.column "old_name", :string
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/002_rename.rb",
            r#"
class Rename < ActiveRecord::Migration[4.2]
  def self.up
    rename_column :widgets, :old_name, :new_name
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "old_name"], "rename must NOT be applied");
        assert_eq!(report.unapplied_mutations, 1);

        let _ = fs::remove_dir_all(&root);
    }

    /// `remove_column` / `change_column` / `drop_table` are each counted,
    /// never applied — mirrors [`classic_rename_column_is_counted_not_applied`]
    /// for the other three mutation kinds in one file, including the
    /// `change_column_default`-must-not-false-match-`change_column` guard.
    #[test]
    fn classic_remove_change_drop_are_counted_not_applied() {
        let root = scratch_dir("mutations");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.column "name", :string
      t.column "legacy", :string
    end
    create_table "gadgets" do |t|
      t.string :name
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/002_mutate.rb",
            r#"
class Mutate < ActiveRecord::Migration[4.2]
  def self.up
    remove_column :widgets, :legacy
    change_column :widgets, :name, :text
    change_column_default :widgets, :name, from: "", to: nil
    drop_table :gadgets
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = tables.iter().find(|t| t.table_name == "widgets").unwrap();
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "name", "legacy"],
            "remove_column must NOT be applied"
        );
        assert_eq!(
            widgets
                .fields
                .iter()
                .find(|f| f.name == "name")
                .unwrap()
                .field_type
                .as_deref(),
            Some("string"),
            "change_column must NOT be applied"
        );
        assert!(
            tables.iter().any(|t| t.table_name == "gadgets"),
            "drop_table must NOT be applied"
        );
        assert_eq!(
            report.unapplied_mutations, 3,
            "remove_column + change_column + drop_table, NOT change_column_default"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A later `create_table` of the same table REPLACES its column list
    /// (idempotent re-create), rather than merging with the earlier one.
    #[test]
    fn classic_later_create_table_replaces_earlier_columns() {
        let root = scratch_dir("recreate");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets" do |t|
      t.string :old_only
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/002_recreate.rb",
            r#"
class Recreate < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.string :new_only
    end
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "new_only"],
            "later create_table must replace, not merge"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// `change_table` never clears — it's an `add_column` batch against an
    /// existing table, not a redefinition.
    #[test]
    fn classic_change_table_appends_without_clearing() {
        let root = scratch_dir("change-table");
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets" do |t|
      t.string :name
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/002_change_table.rb",
            r#"
class AddVerifyPeer < ActiveRecord::Migration[5.2]
  def change
    change_table :widgets do |t|
      t.boolean :verify_peer, default: true, null: false
    end
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "verify_peer"]);
        let by_name = |n: &str| widgets.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("verify_peer").not_null, Some(true));

        let _ = fs::remove_dir_all(&root);
    }

    /// Files are replayed in SORTED filename order, never directory/creation
    /// order — write `002` to disk before `001` and confirm the schema still
    /// reflects `001` having run first.
    #[test]
    fn classic_sorted_file_order_respected() {
        let root = scratch_dir("order");
        write_migration(
            &root,
            "db/migrate/002_recreate.rb",
            r#"
class Recreate < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.string :new_only
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets" do |t|
      t.string :old_only
    end
  end
end
"#,
        );

        let mut report = SchemaReport::default();
        let tables = parse_migrations_dir(&root, &mut report);
        let widgets = &tables[0];
        let names: Vec<&str> = widgets.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "new_only"],
            "001 then 002 must replay in filename order regardless of write order"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Regression: an `OpenProject`-style baseline squash (`db/migrate/tables/`)
    /// alongside a classic `db/migrate/*.rb` file still routes to the
    /// (unchanged) baseline path — the classic file is completely ignored,
    /// not merely deprioritised.
    #[test]
    fn op_layout_takes_priority_over_classic_migrate_dir() {
        let root = scratch_dir("op-priority");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            r#"
class Tables::Widgets < Tables::Base
  def self.table(migration)
    create_table migration do |t|
      t.string :name, null: false
    end
  end
end
"#,
        );
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "gadgets" do |t|
      t.string :name
    end
  end
end
"#,
        );

        let (_, report) = extract_app_with_schema(&root, "redmine");
        assert_eq!(report.columns_from, "baseline-only");
        assert_eq!(
            report.tables_seen, 1,
            "only the baseline table; gadgets must be ignored entirely"
        );
        assert_eq!(report.classic_migrations_scanned, 0);
        assert_eq!(report.unmatched_tables, vec!["widgets".to_string()]);

        let _ = fs::remove_dir_all(&root);
    }

    /// End-to-end: no baseline squash present, only classic migrations —
    /// [`extract_app_with_schema`] routes to the classic surface and merges
    /// the replayed columns into the matching model.
    #[test]
    fn classic_layout_merges_columns_into_matching_model() {
        let root = scratch_dir("classic-merge");
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/001_setup.rb",
            r#"
class Setup < ActiveRecord::Migration[4.2]
  def self.up
    create_table "widgets", :force => true do |t|
      t.column "name", :string, :null => false
    end
  end
end
"#,
        );

        let (graph, report) = extract_app_with_schema(&root, "redmine");
        assert_eq!(report.columns_from, "classic-migrations");
        assert_eq!(report.classic_migrations_scanned, 1);
        assert_eq!(report.tables_matched, 1);
        let widget = graph
            .models
            .iter()
            .find(|m| m.name == "Widget")
            .expect("Widget model");
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name"]);
        assert_eq!(
            widget.fields[1].not_null,
            Some(true),
            "old hash-rocket `:null => false` must be honoured"
        );

        let _ = fs::remove_dir_all(&root);
    }

    // ────────────────── baseline + post-baseline replay (D-AR-3.5) ──────────────────

    /// A baseline table with no post-baseline migrations at all (not even
    /// an empty `db/migrate/` directory) is untouched:
    /// [`SchemaReport::columns_from`] stays `"baseline-only"` — the
    /// behaviour-preserving case the module doc promises.
    #[test]
    fn replay_with_no_post_baseline_migrations_stays_baseline_only() {
        let root = scratch_dir("replay-none");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :name\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );

        let (graph, report) = extract_app_with_schema(&root, "testns");
        assert_eq!(report.columns_from, "baseline-only");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name"]);

        let _ = fs::remove_dir_all(&root);
    }

    /// A post-baseline `add_column` is appended onto the baseline columns,
    /// honouring `null: false`, and flips `columns_from` to
    /// `"baseline+replay"`.
    #[test]
    fn replay_add_column_appends_onto_the_baseline() {
        let root = scratch_dir("replay-add-column");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :name\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_add_price.rb",
            "class AddPrice < ActiveRecord::Migration[7.0]\n  def change\n    add_column :widgets, :price, :integer, null: false\n  end\nend\n",
        );

        let (graph, report) = extract_app_with_schema(&root, "testns");
        assert_eq!(report.columns_from, "baseline+replay");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "price"]);
        let price = widget.fields.iter().find(|f| f.name == "price").unwrap();
        assert_eq!(price.field_type.as_deref(), Some("integer"));
        assert_eq!(price.not_null, Some(true));

        let _ = fs::remove_dir_all(&root);
    }

    /// `rename_column` renames the field IN PLACE — position in the field
    /// list is preserved, not moved to the end.
    #[test]
    fn replay_rename_column_renames_in_place() {
        let root = scratch_dir("replay-rename-column");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :old_name\n      t.string :other\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_rename.rb",
            "class Rename < ActiveRecord::Migration[7.0]\n  def change\n    rename_column :widgets, :old_name, :new_name\n  end\nend\n",
        );

        let (graph, _report) = extract_app_with_schema(&root, "testns");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "new_name", "other"],
            "renamed field keeps its original position"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// `remove_column` (singular) and `remove_columns` (plural, variadic)
    /// both drop fields from the baseline.
    #[test]
    fn replay_remove_column_and_remove_columns_drop_fields() {
        let root = scratch_dir("replay-remove-columns");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :a\n      t.string :b\n      t.string :c\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_remove_a.rb",
            "class RemoveA < ActiveRecord::Migration[7.0]\n  def change\n    remove_column :widgets, :a\n  end\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200103000000_remove_b_and_c.rb",
            "class RemoveBAndC < ActiveRecord::Migration[7.0]\n  def change\n    remove_columns :widgets, :b, :c\n  end\nend\n",
        );

        let (graph, _report) = extract_app_with_schema(&root, "testns");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id"], "a, b, and c must all be dropped");

        let _ = fs::remove_dir_all(&root);
    }

    /// `change_column` updates the field's type — both the bare call form
    /// and the parenthesized form (`change_column(:t, :c, :type)`, the
    /// shape actually used in the `OpenProject` corpus).
    #[test]
    fn replay_change_column_updates_type_bare_and_parenthesized() {
        let root = scratch_dir("replay-change-column");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :title\n      t.integer :count\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_change_title.rb",
            "class ChangeTitle < ActiveRecord::Migration[7.0]\n  def change\n    change_column :widgets, :title, :text\n  end\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200103000000_change_count.rb",
            "class ChangeCount < ActiveRecord::Migration[7.0]\n  def change\n    change_column(:widgets, :count, :bigint, limit:)\n  end\nend\n",
        );

        let (graph, _report) = extract_app_with_schema(&root, "testns");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let by_name = |n: &str| widget.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("title").field_type.as_deref(), Some("text"));
        assert_eq!(
            by_name("count").field_type.as_deref(),
            Some("bigint"),
            "parenthesized change_column call must parse just like the bare form"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// Parenthesized `add_column(:t, :c, :type)` is applied just like the
    /// bare form (codex #73 P2) — the leading `(` must not ride into the
    /// table token and silently drop the mutation.
    #[test]
    fn replay_add_column_parenthesized_form_applies() {
        let root = scratch_dir("replay-add-column-parens");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :name\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_add_price.rb",
            "class AddPrice < ActiveRecord::Migration[7.0]\n  def change\n    add_column(:widgets, :price, :integer)\n  end\nend\n",
        );

        let (graph, report) = extract_app_with_schema(&root, "testns");
        assert_eq!(report.columns_from, "baseline+replay");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let price = widget.fields.iter().find(|f| f.name == "price");
        assert!(price.is_some(), "parenthesized add_column must be applied");
        assert_eq!(price.unwrap().field_type.as_deref(), Some("integer"));

        let _ = fs::remove_dir_all(&root);
    }

    /// A `def up` / `def down` migration replays only the `up` direction
    /// (codex #73 P2) — the `down` rollback (which removes the just-added
    /// column) must be skipped, or `baseline+replay` would drop a valid
    /// column.
    #[test]
    fn replay_skips_def_down_rollback_body() {
        let root = scratch_dir("replay-def-down");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :name\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_add_foo.rb",
            "class AddFoo < ActiveRecord::Migration[7.0]\n  def up\n    add_column :widgets, :foo, :string\n  end\n\n  def down\n    remove_column :widgets, :foo\n  end\nend\n",
        );

        let (graph, report) = extract_app_with_schema(&root, "testns");
        assert_eq!(report.columns_from, "baseline+replay");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "name", "foo"],
            "the `up` add must survive; the `down` remove must be skipped"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// `modules/*/db/migrate/*.rb` is scanned too, not just the core
    /// `db/migrate/`.
    #[test]
    fn replay_scans_module_migrations_too() {
        let root = scratch_dir("replay-module-migrations");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :name\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "modules/gadgets/db/migrate/20200102000000_add_sku.rb",
            "class AddSku < ActiveRecord::Migration[7.0]\n  def change\n    add_column :widgets, :sku, :string\n  end\nend\n",
        );

        let (graph, report) = extract_app_with_schema(&root, "testns");
        assert_eq!(report.columns_from, "baseline+replay");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name", "sku"]);

        let _ = fs::remove_dir_all(&root);
    }

    /// Core (`db/migrate/`) and module (`modules/*/db/migrate/`) migrations
    /// are replayed in ONE combined filename-sorted order, not
    /// "core dir first, then module dirs" — a module migration
    /// timestamped BEFORE a core migration must still apply first.
    #[test]
    fn replay_orders_core_and_module_migrations_by_filename_across_both_dirs() {
        let root = scratch_dir("replay-cross-dir-order");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :v\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        // Earlier timestamp, lives under modules/ — must apply FIRST.
        write_migration(
            &root,
            "modules/gadgets/db/migrate/20200101000000_step1.rb",
            "class Step1 < ActiveRecord::Migration[7.0]\n  def change\n    rename_column :widgets, :v, :v_mid\n  end\nend\n",
        );
        // Later timestamp, lives under core db/migrate/ — must apply SECOND.
        // A path-based (not filename-based) sort would run this FIRST
        // (`"db/..."` < `"modules/..."` lexicographically), in which case
        // it would find no `v_mid` column yet and no-op, leaving the field
        // named `v_mid` instead of `v_final`.
        write_migration(
            &root,
            "db/migrate/20200102000000_step2.rb",
            "class Step2 < ActiveRecord::Migration[7.0]\n  def change\n    rename_column :widgets, :v_mid, :v_final\n  end\nend\n",
        );

        let (graph, _report) = extract_app_with_schema(&root, "testns");
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(
            names,
            ["id", "v_final"],
            "timestamp order must win over directory order"
        );

        let _ = fs::remove_dir_all(&root);
    }

    /// A mutation naming a table the baseline squash never produced is a
    /// no-op: it neither crashes nor invents a new table/model entry.
    #[test]
    fn replay_mutation_on_unknown_table_is_a_no_op() {
        let root = scratch_dir("replay-unknown-table");
        write_migration(
            &root,
            "db/migrate/tables/widgets.rb",
            "class Tables::Widgets < Tables::Base\n  def self.table(migration)\n    create_table migration do |t|\n      t.string :name\n    end\n  end\nend\n",
        );
        write_migration(
            &root,
            "app/models/widget.rb",
            "class Widget < ActiveRecord::Base\nend\n",
        );
        write_migration(
            &root,
            "db/migrate/20200102000000_add_to_gadgets.rb",
            "class AddToGadgets < ActiveRecord::Migration[7.0]\n  def change\n    add_column :gadgets, :foo, :string\n  end\nend\n",
        );

        let (graph, report) = extract_app_with_schema(&root, "testns");
        assert_eq!(
            report.columns_from, "baseline-only",
            "a mutation matching no baseline table applies nothing"
        );
        assert_eq!(report.tables_seen, 1, "gadgets must not be invented");
        assert!(report.unmatched_tables.is_empty());
        let widget = graph.models.iter().find(|m| m.name == "Widget").unwrap();
        let names: Vec<&str> = widget.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "name"]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn inflection_covers_the_corpus_shapes() {
        for (table, model) in [
            ("work_packages", "WorkPackage"),
            ("statuses", "Status"),
            ("categories", "Category"),
            ("queries", "Query"),
            ("changes", "Change"),
            ("changesets", "Changeset"),
            ("news", "News"),
            ("attachments", "Attachment"),
            ("custom_fields", "CustomField"),
            ("issue_priorities", "IssuePriority"),
        ] {
            assert_eq!(model_name_for_table(table), model, "{table}");
        }
    }

    /// Corpus gate (same pattern as the crate's D-AR-4 gate): only runs
    /// with `OPENPROJECT_PATH` set. Pins the `WorkPackage` baseline+replay
    /// column set — the oracle-diff ground truth, now including the four
    /// post-baseline mutation kinds [`replay_post_baseline_migrations`]
    /// applies (module doc: "Post-baseline replay").
    ///
    /// `columns_from` flips to `"baseline+replay"` here: the real corpus
    /// has post-baseline `add_column`s landing on `work_packages`
    /// (`position`/`story_points`/`remaining_hours` from the `backlogs`
    /// module's aggregated migration, `budget_id` from `budgets`'s) — net
    /// +4 over the 27-column pre-replay baseline this test used to pin
    /// (`sequence_number`/`identifier` are also added, by
    /// `20260330100000_create_work_package_semantic_ids.rb`, but the SAME
    /// file `remove_column`s them a few lines down, so they correctly net
    /// to zero — see [`replay_migration_source`]'s doc).
    #[test]
    #[allow(clippy::print_stderr)] // diagnostic emission gated on env var (real-corpus gate)
    fn openproject_corpus_schema_gate() {
        let Ok(root) = std::env::var("OPENPROJECT_PATH") else {
            eprintln!("skipping: OPENPROJECT_PATH not set");
            return;
        };
        let (graph, report) = extract_app_with_schema(Path::new(&root), "openproject");
        assert_eq!(report.columns_from, "baseline+replay");
        assert!(
            report.tables_seen >= 90,
            "expected ~99 baseline tables, saw {}",
            report.tables_seen
        );
        assert!(
            report.tables_matched >= 50,
            "matched only {}",
            report.tables_matched
        );
        eprintln!(
            "D-AR-3.5 schema gate: {} tables seen, {} matched, {} unmatched, {} files skipped",
            report.tables_seen,
            report.tables_matched,
            report.unmatched_tables.len(),
            report.files_skipped.len()
        );

        let wp = graph
            .models
            .iter()
            .find(|m| m.name == "WorkPackage")
            .expect("WorkPackage model");
        let cols: Vec<&str> = wp
            .fields
            .iter()
            .filter(|f| f.field_type.is_some())
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(
            cols.len(),
            33,
            "baseline+replay WorkPackage columns: {cols:?}"
        );
        for expected in [
            "id",
            "type_id",
            "project_id",
            "subject",
            "description",
            "due_date",
            "category_id",
            "status_id",
            "assigned_to_id",
            "priority_id",
            "version_id",
            "author_id",
            "lock_version",
            "done_ratio",
            "estimated_hours",
            "created_at",
            "updated_at",
            "start_date",
            "responsible_id",
            "derived_estimated_hours",
            "schedule_manually",
            "parent_id",
            "duration",
            "ignore_non_working_days",
            "derived_remaining_hours",
            "derived_done_ratio",
            "project_phase_id",
            // Post-baseline replay additions.
            "position",
            "story_points",
            "remaining_hours",
            "budget_id",
            // Added by `20260330100000_create_work_package_semantic_ids.rb`'s
            // `def up`; its `def down` removes them, but Rails applies only the
            // `up` direction (codex #73 P2 — the replay now skips `def down`
            // bodies, so these are correctly RETAINED, not netted to absent).
            "sequence_number",
            "identifier",
        ] {
            assert!(cols.contains(&expected), "missing column {expected}");
        }
        let by_name = |n: &str| wp.fields.iter().find(|f| f.name == n).unwrap();
        assert_eq!(by_name("subject").field_type.as_deref(), Some("string"));
        assert_eq!(by_name("subject").not_null, Some(true));
        assert_eq!(by_name("done_ratio").field_type.as_deref(), Some("integer"));
        assert_eq!(
            by_name("done_ratio").not_null,
            None,
            "unset ≠ 0% — the oracle-diff bug"
        );
        assert_eq!(by_name("schedule_manually").not_null, Some(true));
        assert_eq!(by_name("budget_id").field_type.as_deref(), Some("integer"));
        assert!(
            cols.contains(&"sequence_number") && cols.contains(&"identifier"),
            "`def up` add must survive; the `def down` remove is skipped (codex #73 P2): {cols:?}"
        );
    }

    /// **D-AR-3.5 drift fuse** for [`replay_post_baseline_migrations`], in
    /// the house-style env-gate + self-skip shape (`RAILS_CORPUS_SRC` +
    /// optional `RAILS_CORPUS_NS`, default namespace `"openproject"`):
    /// `RAILS_CORPUS_SRC=/home/user/openproject cargo test -p ruff_ruby_spo
    /// -- --nocapture`.
    ///
    /// **Honest result vs. the wave's working assumption:** this wave's
    /// brief assumed a 40-field pre-replay baseline and expected replay to
    /// push `WorkPackage` past 64 fields, toward a ~109-field "real" shape.
    /// Measured against the actual corpus at the time this fuse was
    /// pinned, the PRE-replay baseline is 27 fields (matching
    /// [`openproject_corpus_schema_gate`]'s long-standing pin — 40 does not
    /// reproduce), and POST-replay is 33: a real, verified +6 — four genuine
    /// post-baseline `add_column`s (`position`/`story_points`/`remaining_hours`
    /// from the `backlogs` module, `budget_id` from `budgets`) PLUS
    /// `sequence_number`/`identifier` from `create_work_package_semantic_ids`'s
    /// `def up` (its `def down` removes them, but Rails applies only `up`, and
    /// the replay now skips `def down` bodies — codex #73 P2; the earlier
    /// "net to zero" pin of 31 was the bug the reviewer caught).
    /// It does not reach 64. Investigation (see the wave report) found the
    /// remaining gap lives almost entirely in forms this replay pass
    /// deliberately does not cover — chiefly a `change_table :work_packages
    /// do |t| … end` block
    /// (`db/migrate/20250403150639_link_wp_to_project_phase_definition.rb`)
    /// — which is out of scope for a bounded, four-top-level-statement
    /// replay (module doc: "Post-baseline replay"). Pinning the true
    /// measured number here rather than a hoped-for one, per this module's
    /// conservation-ledger discipline: a silent regression below 33 should
    /// trip this fuse just as loudly as a silent jump would.
    #[test]
    #[allow(clippy::print_stderr)] // diagnostic emission gated on env var (real-corpus gate)
    fn rails_corpus_baseline_replay_drift_fuse() {
        let Ok(root) = std::env::var("RAILS_CORPUS_SRC") else {
            eprintln!("skipping: RAILS_CORPUS_SRC not set");
            return;
        };
        let namespace =
            std::env::var("RAILS_CORPUS_NS").unwrap_or_else(|_| "openproject".to_string());
        let (graph, report) = extract_app_with_schema(Path::new(&root), &namespace);

        let wp = graph
            .models
            .iter()
            .find(|m| m.name == "WorkPackage")
            .expect("WorkPackage model");
        let field_count = wp.fields.len();
        eprintln!(
            "D-AR-3.5 baseline+replay drift fuse: WorkPackage field count = {field_count} \
             (pre-replay baseline was 27; columns_from = {})",
            report.columns_from
        );

        assert_eq!(
            report.columns_from, "baseline+replay",
            "expected at least one post-baseline mutation to be replayed somewhere in the corpus"
        );
        assert!(
            field_count > 27,
            "expected post-baseline replay to add at least one field over the 27-field \
             pre-replay baseline, got {field_count}"
        );
        assert_eq!(
            field_count, 33,
            "drift fuse: WorkPackage field count via baseline+replay on the real OpenProject \
             corpus moved away from the pinned 33 — confirm whether the corpus or the parser \
             changed before updating this pin (see doc comment: this is short of the wave's \
             hoped-for >64/~109, by design scope, not a bug)"
        );
    }

    // ────────────────── compute-linkage pass (D-AR-3.5) ──────────────────

    use ruff_spo_triplet::Function;

    /// A `compute_<x>` def whose class also has an `<x>` field (schema-merged
    /// or otherwise) gets linked: `field.emitted_by = Some("compute_<x>")`.
    #[test]
    fn link_computed_fields_links_matching_compute_def() {
        let mut model = Model {
            name: "WorkPackage".to_string(),
            fields: vec![Field {
                name: "total_hours".to_string(),
                ..Field::default()
            }],
            functions: vec![Function {
                name: "compute_total_hours".to_string(),
                ..Function::default()
            }],
            ..Model::default()
        };
        link_computed_fields(&mut model);
        assert_eq!(
            model.fields[0].emitted_by.as_deref(),
            Some("compute_total_hours")
        );
    }

    /// The guardrail: a `compute_<x>` def with NO matching `<x>` field must
    /// NOT synthesize a field — linkage requires the field to already exist
    /// on both sides (schema stratum + method-name stratum), never just the
    /// method name alone.
    #[test]
    fn link_computed_fields_does_not_synthesize_a_field_for_an_unmatched_compute_def() {
        let mut model = Model {
            name: "WorkPackage".to_string(),
            fields: Vec::new(),
            functions: vec![Function {
                name: "compute_total_hours".to_string(),
                ..Function::default()
            }],
            ..Model::default()
        };
        link_computed_fields(&mut model);
        assert!(
            model.fields.is_empty(),
            "a compute def with no matching field must not create one"
        );
    }

    /// A field with no matching `compute_<x>` def stays unlinked —
    /// `emitted_by` remains `None`.
    #[test]
    fn link_computed_fields_leaves_uncomputed_fields_alone() {
        let mut model = Model {
            name: "WorkPackage".to_string(),
            fields: vec![Field {
                name: "subject".to_string(),
                ..Field::default()
            }],
            functions: Vec::new(),
            ..Model::default()
        };
        link_computed_fields(&mut model);
        assert_eq!(model.fields[0].emitted_by, None);
    }
}
