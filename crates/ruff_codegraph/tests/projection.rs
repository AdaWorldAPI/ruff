//! The projection against two revisions of a small synthetic Rails tree.
//!
//! `data/rev1` and `data/rev2` differ in two ways only: two `before_save`
//! callbacks of `Ticket` swap places, and `Ticket#close` loses its
//! `self.save!` call. `data/expected/<rev>` holds the store files each
//! revision must project to; `RUFF_CODEGRAPH_BLESS=1` rewrites them.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use ruff_codegraph::account;
use ruff_codegraph::error::Error;
use ruff_codegraph::receipt::Receipt;
use ruff_codegraph::rows::{self, Builder};
use ruff_codegraph::{Config, Projection, Relation, project, relation};
use ruff_ruby_spo::extract_tree_with;
use ruff_spo_triplet::{Callback, Function, Model, ModelGraph};
use tempfile::TempDir;

const FILES: [&str; 5] = [
    "shop.rows.tsv",
    "shop.files.tsv",
    "shop.residuals.tsv",
    "shop.events.tsv",
    "shop.receipt.json",
];

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn config(rev: &str) -> PathBuf {
    data().join(rev).join("config.toml")
}

fn run(rev: &str) -> TempDir {
    let out = TempDir::new().unwrap();
    project(&config(rev), out.path()).unwrap();
    out
}

fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A copy of a revision in a temp dir with its config text rewritten.
fn variant(rev: &str, edit: impl Fn(&str) -> String) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    copy_tree(&data().join(rev), dir.path());
    let config = dir.path().join("config.toml");
    let text = fs::read_to_string(&config).unwrap();
    fs::write(&config, edit(&text)).unwrap();
    (dir, config)
}

/// The data lines of a TSV file, header excluded.
fn lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.starts_with('#')).collect()
}

fn count_lines(text: &str, wanted: &str) -> usize {
    lines(text).into_iter().filter(|l| *l == wanted).count()
}

fn count_prefix(text: &str, prefix: &str) -> usize {
    lines(text)
        .into_iter()
        .filter(|l| l.starts_with(prefix))
        .count()
}

const G: &str = "SOURCE_STRUCTURE";
const P: &str = "SOURCE_STRUCTURE+POSSIBLE";

// ---------------------------------------------------------------------------
// An independent check of the reader's rules, written from the reader's source
// (`lance-graph-codegraph` `artifact.rs`), not from this crate.
// ---------------------------------------------------------------------------

const RESIDUAL_REASONS: [&str; 5] = [
    "BODY_NOT_STRUCTURED",
    "CALLEE_NOT_NAMED",
    "REFERENCE_NOT_NAMED",
    "TRAIT_METHOD_NOT_FOUND",
    "TRAIT_IMPL_METHOD_NOT_FOUND",
];

fn header(text: &str, version: u32) -> Result<(Vec<&str>, Vec<&str>), String> {
    let mut it = text.lines();
    if it.next() != Some(format!("#version {version}").as_str()) {
        return Err(format!("first line is not `#version {version}`"));
    }
    let schema = it
        .next()
        .and_then(|l| l.strip_prefix("#schema "))
        .ok_or("no #schema line")?;
    Ok((schema.split('\t').collect(), it.collect()))
}

fn span(cell: &str) -> bool {
    cell == "-" || cell.parse::<u32>().is_ok()
}

fn has_placeholder(name: &str) -> bool {
    name.split("::").any(|segment| segment == "?")
}

fn validate_rows(text: &str) -> Result<(), String> {
    const REQUIRED: [&str; 7] = [
        "predicate",
        "subject",
        "object",
        "grade",
        "file",
        "start",
        "end",
    ];
    let (names, body) = header(text, 2)?;
    let mut at = [0usize; 7];
    for (slot, want) in at.iter_mut().zip(REQUIRED) {
        *slot = names
            .iter()
            .position(|name| *name == want)
            .ok_or(format!("schema lacks {want}"))?;
    }
    let [predicate, subject, object, grade, file, start, end] = at;
    for (i, line) in body.iter().enumerate() {
        let lineno = i + 3;
        if line.is_empty() {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        if cells.len() != names.len() {
            return Err(format!(
                "line {lineno}: {} cells, schema has {}",
                cells.len(),
                names.len()
            ));
        }
        if !span(cells[start]) || !span(cells[end]) {
            return Err(format!("line {lineno}: span cell is neither `-` nor a u32"));
        }
        if cells[predicate].is_empty() || cells[grade].is_empty() || cells[file].is_empty() {
            return Err(format!("line {lineno}: empty predicate, grade or file"));
        }
        let placeholder_checked = matches!(
            cells[predicate],
            "DEFINES" | "CALLS_RESOLVED" | "PUBLIC" | "IMPLEMENTS" | "SYNTHETIC" | "REFERENCES"
        );
        if placeholder_checked
            && (has_placeholder(cells[subject]) || has_placeholder(cells[object]))
        {
            return Err(format!("line {lineno}: placeholder symbol"));
        }
        if cells[predicate] == "UNRESOLVED_CALL" && has_placeholder(cells[subject]) {
            return Err(format!("line {lineno}: placeholder subject"));
        }
    }
    Ok(())
}

fn validate_residuals(text: &str) -> Result<(), String> {
    let mut it = text.lines();
    if it.next() != Some("#version 1") {
        return Err("version is not 1".into());
    }
    if it.next() != Some("#schema reason\tsubject") {
        return Err("schema is not `reason\\tsubject`".into());
    }
    for (i, line) in it.enumerate() {
        let cells: Vec<&str> = line.split('\t').collect();
        if cells.len() != 2 {
            return Err(format!("line {}: arity", i + 3));
        }
        if !RESIDUAL_REASONS.contains(&cells[0]) {
            return Err(format!("line {}: unknown reason {}", i + 3, cells[0]));
        }
    }
    Ok(())
}

fn validate_files(text: &str) -> Result<(), String> {
    let (names, body) = header(text, 1)?;
    if names != ["path", "fnv64"] {
        return Err("schema is not path, fnv64".into());
    }
    for line in body {
        match line.split('\t').collect::<Vec<_>>()[..] {
            [path, digest]
                if !path.is_empty()
                    && digest.len() == 16
                    && digest.chars().all(|c| c.is_ascii_hexdigit()) => {}
            _ => return Err(format!("bad files line {line:?}")),
        }
    }
    Ok(())
}

fn validate_events(text: &str) -> Result<(), String> {
    let (names, body) = header(text, 2)?;
    if names
        != [
            "scope", "seq", "lane", "kind", "subject", "object", "anchor",
        ]
    {
        return Err("schema is not the events schema".into());
    }
    for (i, line) in body.iter().enumerate() {
        let cells: Vec<&str> = line.split('\t').collect();
        if cells.len() != 7
            || cells[1].parse::<u32>().is_err()
            || !["DECL", "BODY", "UNREAD"].contains(&cells[2])
            || cells[6].parse::<u32>().is_err()
        {
            return Err(format!("line {}: not an event row", i + 3));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------

#[test]
fn golden_equality_and_a_second_run_gives_identical_bytes() {
    let bless = std::env::var("RUFF_CODEGRAPH_BLESS").is_ok_and(|v| v == "1");
    for rev in ["rev1", "rev2"] {
        let first = run(rev);
        let second = run(rev);
        let expected = data().join("expected").join(rev);
        for name in FILES {
            let got = read(first.path(), name);
            assert_eq!(
                got,
                read(second.path(), name),
                "{rev}/{name} differs between two runs"
            );
            if bless {
                fs::create_dir_all(&expected).unwrap();
                fs::write(expected.join(name), &got).unwrap();
            }
            let want = fs::read_to_string(expected.join(name)).unwrap_or_else(|e| {
                panic!("missing {rev}/{name}: {e}; run with RUFF_CODEGRAPH_BLESS=1")
            });
            assert_eq!(got, want, "{rev}/{name} differs from the committed file");
        }
    }
}

#[test]
fn the_files_pass_the_readers_rules() {
    for rev in ["rev1", "rev2"] {
        let out = run(rev);
        validate_rows(&read(out.path(), "shop.rows.tsv")).unwrap();
        validate_residuals(&read(out.path(), "shop.residuals.tsv")).unwrap();
        validate_files(&read(out.path(), "shop.files.tsv")).unwrap();
        validate_events(&read(out.path(), "shop.events.tsv")).unwrap();
    }
}

#[test]
fn the_validator_can_fail() {
    assert!(
        validate_rows("#version 1\n#schema predicate\tsubject\tobject\tgrade\tfile\tstart\tend\n")
            .is_err()
    );
    assert!(validate_rows("#version 2\n#schema predicate\tsubject\n").is_err());
    let schema = "#version 2\n#schema predicate\tsubject\tobject\tgrade\tfile\tstart\tend\n";
    assert!(validate_rows(&format!("{schema}DEFINES\ta\tb\tg\t-\t-\n")).is_err());
    assert!(validate_rows(&format!("{schema}DEFINES\ta\tb\tg\t-\tx\t1\n")).is_err());
    assert!(validate_rows(&format!("{schema}DEFINES\ta::?\tb\tg\t-\t-\t-\n")).is_err());
    assert!(validate_rows(&format!("{schema}DEFINES\ta\tb\tg\t-\t-\t-\n")).is_ok());
    assert!(validate_residuals("#version 1\n#schema reason\tsubject\nSOMETHING\ta\n").is_err());
    assert!(validate_files("#version 1\n#schema path\tfnv64\na.rb\t12\n").is_err());
    assert!(validate_events("#version 1\n#schema scope\tseq\tlane\tkind\tsubject\tobject\tanchor\nS\t0\tNOPE\tk\ts\to\t1\n").is_err());
}

#[test]
fn a_callback_swap_changes_the_order_lane_and_no_structural_row() {
    let (one, two) = (run("rev1"), run("rev2"));
    let rows_one = read(one.path(), "shop.rows.tsv");
    let rows_two = read(two.path(), "shop.rows.tsv");

    let class_rows = |text: &str| -> BTreeSet<String> {
        lines(text)
            .into_iter()
            .filter(|l| {
                l.split('\t')
                    .nth(1)
                    .is_some_and(|s| s.starts_with("shop::Ticket"))
            })
            .map(str::to_owned)
            .collect()
    };
    let removed = format!("UNRESOLVED_CALL\tshop::Ticket::close\tself.save!\t{P}\t-\t-\t-");
    let mut expected = class_rows(&rows_one);
    assert!(
        expected.remove(&removed),
        "rev1 must hold the call rev2 drops"
    );
    assert_eq!(class_rows(&rows_two), expected);
    // The swap is invisible to every other row too.
    let all_one: BTreeSet<&str> = lines(&rows_one).into_iter().collect();
    let all_two: BTreeSet<&str> = lines(&rows_two).into_iter().collect();
    assert_eq!(
        all_one.difference(&all_two).copied().collect::<Vec<_>>(),
        [removed.as_str()]
    );
    assert_eq!(all_two.difference(&all_one).count(), 0);

    let ask = |dir: &TempDir| {
        relation(
            dir.path(),
            "shop",
            "shop::Ticket",
            "normalize_title",
            "stamp_owner",
        )
        .unwrap()
    };
    assert_eq!(ask(&one), Relation::Before);
    assert_eq!(ask(&two), Relation::After);
    let reverse = relation(
        one.path(),
        "shop",
        "shop::Ticket",
        "stamp_owner",
        "normalize_title",
    )
    .unwrap();
    assert_eq!(reverse, Relation::After);
}

#[test]
fn the_order_question_answers_unknown_when_it_cannot_say() {
    let out = run("rev1");
    let ask = |scope: &str, a: &str, b: &str| relation(out.path(), "shop", scope, a, b).unwrap();
    assert_eq!(
        ask("shop::Ticket", "normalize_title", "no_such_callback"),
        Relation::Unknown
    );
    assert_eq!(
        ask("shop::NoSuchClass", "normalize_title", "stamp_owner"),
        Relation::Unknown
    );
    assert_eq!(
        ask("shop::Ticket", "normalize_title", "normalize_title"),
        Relation::Unknown
    );
    // A name need only be present in the scope to answer.
    assert_eq!(
        ask("shop::Ticket::close", "state", "touch"),
        Relation::Before
    );
    assert_eq!(
        ask("shop::Ticket::close", "touch", "save!"),
        Relation::Before
    );
    // `notify` is declared twice in `Comment`, both times after `body`.
    assert_eq!(ask("shop::Comment", "body", "notify"), Relation::Before);
}

#[test]
fn occurrences_that_interleave_make_the_answer_unknown() {
    let out = run("rev1");
    let row = |seq: u32, object: &str| {
        format!("S\t{seq}\tDECL\tCallback\tbefore_save\t{object}\t{seq}\n")
    };
    let events = |objects: &[&str]| {
        let mut text =
            String::from("#version 2\n#schema scope\tseq\tlane\tkind\tsubject\tobject\tanchor\n");
        for (i, object) in objects.iter().enumerate() {
            text.push_str(&row(u32::try_from(i).unwrap(), object));
        }
        text
    };
    let ask = |objects: &[&str]| {
        fs::write(out.path().join("shop.events.tsv"), events(objects)).unwrap();
        relation(out.path(), "shop", "S", "a", "b").unwrap()
    };
    assert_eq!(ask(&["a", "b", "a"]), Relation::Unknown);
    assert_eq!(ask(&["b", "a", "b"]), Relation::Unknown);
    // A repeated name is not ambiguous while all its occurrences are on one side.
    assert_eq!(ask(&["a", "a", "b"]), Relation::Before);
    assert_eq!(ask(&["b", "a", "a"]), Relation::After);
    // An `UNREAD` row is not an event: a name in it does not count as an
    // occurrence. Read as an event it would make `b` interleave with `a`.
    let unread = "#version 2\n#schema scope\tseq\tlane\tkind\tsubject\tobject\tanchor\n\
        S\t0\tUNREAD\tOTHER\tb\t-\t0\n\
        S\t0\tDECL\tCallback\tbefore_save\ta\t1\n\
        S\t1\tDECL\tCallback\tbefore_save\tb\t2\n";
    fs::write(out.path().join("shop.events.tsv"), unread).unwrap();
    assert_eq!(
        relation(out.path(), "shop", "S", "a", "b").unwrap(),
        Relation::Before
    );
}

#[test]
fn without_the_order_lane_the_question_is_unknown() {
    let (_tree, config) = variant("rev2", |t| t.replace("order = true", "order = false"));
    let out = TempDir::new().unwrap();
    // A stale events file from an earlier run of the same store must not be read.
    project(&config_of("rev2"), out.path()).unwrap();
    assert!(out.path().join("shop.events.tsv").exists());
    assert_eq!(
        relation(
            out.path(),
            "shop",
            "shop::Ticket",
            "stamp_owner",
            "normalize_title"
        )
        .unwrap(),
        Relation::Before
    );
    let receipt = project(&config, out.path()).unwrap();

    assert!(!out.path().join("shop.events.tsv").exists());
    assert_eq!(receipt.lanes["order"], "absent");
    assert_eq!(receipt.lanes["structural"], "present");
    assert!(receipt.cardinality.events.is_empty());
    assert!(!receipt.formats.contains_key("events"));
    let on_disk: Receipt = serde_json::from_str(&read(out.path(), "shop.receipt.json")).unwrap();
    assert_eq!(on_disk, receipt);
    for (a, b) in [
        ("normalize_title", "stamp_owner"),
        ("stamp_owner", "normalize_title"),
    ] {
        assert_eq!(
            relation(out.path(), "shop", "shop::Ticket", a, b).unwrap(),
            Relation::Unknown
        );
    }
}

fn config_of(rev: &str) -> PathBuf {
    config(rev)
}

#[test]
fn class_form_callback_and_both_validation_forms_are_rows() {
    let out = run("rev1");
    let rows = read(out.path(), "shop.rows.tsv");

    // Callbacks: four on Ticket (two `before_save`, the class-form
    // `after_create`, `after_commit`) and one on Comment.
    assert_eq!(count_prefix(&rows, "CALLBACK\t"), 5);
    assert_eq!(count_prefix(&rows, "CALLBACK\tshop::Ticket::"), 4);
    // The class form (`AuditCallback.new`) names its class.
    assert_eq!(
        count_lines(
            &rows,
            &format!("CALLBACK\tshop::Ticket::<after_create>\tAuditCallback\t{G}\t-\t-\t-")
        ),
        1
    );
    assert_eq!(
        count_lines(
            &rows,
            &format!(
                "CALLBACK_OPTION\tshop::Ticket::<after_commit>\tannounce:if=:open?\t{G}\t-\t-\t-"
            )
        ),
        1
    );

    // Validations: `validate :check_state` and the two `validates :title`
    // declarations (one row, two option rows) are VALIDATES rows; Comment has one.
    assert_eq!(count_prefix(&rows, "VALIDATES\t"), 4);
    assert_eq!(
        count_lines(
            &rows,
            &format!("VALIDATES\tshop::Ticket\tvalidate:check_state\t{G}\t-\t-\t-")
        ),
        1
    );
    assert_eq!(
        count_lines(
            &rows,
            &format!("VALIDATES\tshop::Ticket\tvalidates:title\t{G}\t-\t-\t-")
        ),
        1
    );
    assert_eq!(count_prefix(&rows, "VALIDATES_OPTION\tshop::Ticket\t"), 2);
    assert_eq!(count_prefix(&rows, "DSL_CALL\t"), 0);
    assert_eq!(
        count_lines(
            &rows,
            &format!("VALIDATES\tshop::Ticket\tvalidates_with:StateValidator\t{G}\t-\t-\t-")
        ),
        1
    );

    // Residuals: the callback class and the undefined `announce` target.
    let residuals = read(out.path(), "shop.residuals.tsv");
    assert_eq!(
        count_lines(
            &residuals,
            "REFERENCE_NOT_NAMED\tshop::Ticket::<after_create>"
        ),
        1
    );
    assert_eq!(
        count_lines(
            &residuals,
            "REFERENCE_NOT_NAMED\tshop::Ticket::<after_commit>"
        ),
        1
    );
    assert_eq!(count_prefix(&residuals, "REFERENCE_NOT_NAMED\t"), 2);
    // Comment's `notify` is a method of Comment: no residual for it.
    assert_eq!(
        count_prefix(&residuals, "REFERENCE_NOT_NAMED\tshop::Comment"),
        0
    );
}

#[test]
fn a_name_match_is_a_possible_call_and_the_rest_are_unresolved() {
    let out = run("rev1");
    let rows = read(out.path(), "shop.rows.tsv");
    let residuals = read(out.path(), "shop.residuals.tsv");

    assert_eq!(
        count_lines(
            &rows,
            &format!("CALLS_NAMED\tshop::Ticket::close\tshop::Ticket::touch\t{P}\t-\t-\t-")
        ),
        1
    );
    assert_eq!(
        count_lines(
            &rows,
            &format!("UNRESOLVED_CALL\tshop::Ticket::close\tself.save!\t{P}\t-\t-\t-")
        ),
        1
    );
    assert_eq!(
        count_lines(
            &rows,
            &format!("UNRESOLVED_CALL\tshop::Comment::notify\tticket.update\t{P}\t-\t-\t-")
        ),
        1
    );
    assert_eq!(count_prefix(&rows, "CALLS_NAMED\t"), 1);
    assert_eq!(count_prefix(&rows, "UNRESOLVED_CALL\t"), 2);
    assert_eq!(
        count_lines(&residuals, "CALLEE_NOT_NAMED\tshop::Ticket::close"),
        1
    );
    assert_eq!(
        count_lines(&residuals, "CALLEE_NOT_NAMED\tshop::Comment::notify"),
        1
    );
    assert_eq!(count_prefix(&residuals, "CALLEE_NOT_NAMED\t"), 2);
    for rev in ["rev1", "rev2"] {
        let out = run(rev);
        assert!(!read(out.path(), "shop.rows.tsv").contains("CALLS_RESOLVED"));
    }
}

fn hand_model(name: &str, functions: Vec<Function>) -> ModelGraph {
    let mut model = Model::new(name);
    model.functions = functions;
    ModelGraph {
        namespace: "t".to_owned(),
        models: vec![model],
    }
}

fn function(name: &str, calls: &[&str]) -> Function {
    let mut f = Function {
        name: name.to_owned(),
        ..Function::default()
    };
    f.calls = calls.iter().map(|c| (*c).to_owned()).collect();
    f
}

fn build(graph: &ModelGraph) -> Result<rows::Built, Error> {
    let mut builder = Builder::new(&graph.namespace)?;
    rows::structural(graph, &mut builder)?;
    Ok(builder.finish())
}

fn predicates(built: &rows::Built, predicate: &str) -> Vec<(String, String)> {
    built
        .rows
        .iter()
        .filter(|r| r.predicate == predicate)
        .map(|r| (r.subject.clone(), r.object.clone()))
        .collect()
}

#[test]
fn only_a_self_call_naming_exactly_one_function_is_a_named_call() {
    let graph = hand_model(
        "M",
        vec![
            function("caller", &["self.touch", "order.update", "self.save"]),
            function("touch", &[]),
            // Two definitions of `update`, so `self.update` is ambiguous.
            function("update", &[]),
            function("update", &[]),
            function("other", &["self.update"]),
        ],
    );
    let built = build(&graph).unwrap();
    assert_eq!(
        predicates(&built, "CALLS_NAMED"),
        [("t::M::caller".to_owned(), "t::M::touch".to_owned())]
    );
    let unresolved: Vec<String> = predicates(&built, "UNRESOLVED_CALL")
        .into_iter()
        .map(|(s, o)| format!("{s} {o}"))
        .collect();
    assert_eq!(
        unresolved,
        [
            "t::M::caller order.update",
            "t::M::caller self.save",
            "t::M::other self.update"
        ]
    );
    assert_eq!(built.residuals.len(), 3);
    account::check(Some(&graph), None, &built).unwrap();
}

#[test]
fn conservation_fails_on_a_short_row_set() {
    let graph = extract_tree_with(&data().join("rev1/app/models"), "shop");
    let built = build(&graph).unwrap();
    account::check(Some(&graph), None, &built).unwrap();

    let drop_row = |predicate: &str| {
        let mut short = built.clone();
        let at = short
            .rows
            .iter()
            .position(|r| r.predicate == predicate)
            .unwrap();
        short.rows.remove(at);
        account::check(Some(&graph), None, &short)
    };
    for (predicate, family) in [
        ("CALLBACK", "callbacks"),
        ("VALIDATES", "validations"),
        ("ASSOCIATION", "associations"),
        ("UNRESOLVED_CALL", "calls"),
    ] {
        let Err(Error::Conservation(items)) = drop_row(predicate) else {
            panic!("dropping a {predicate} row must break conservation");
        };
        assert!(
            items.iter().any(|i| i.starts_with(family)),
            "{items:?} must name {family}"
        );
    }

    let mut no_residual = built.clone();
    no_residual.residuals.pop();
    assert!(matches!(
        account::check(Some(&graph), None, &no_residual),
        Err(Error::Conservation(_))
    ));
}

#[test]
fn the_conservation_check_stops_a_projection_before_anything_is_written() {
    // A hand graph whose function carries a name the writer refuses fails the
    // run; nothing reaches the store.
    let graph = hand_model("M", vec![function("bad\tname", &[])]);
    assert!(matches!(build(&graph), Err(Error::BadCell { .. })));
}

#[test]
fn repeated_events_stay_repeated() {
    let out = run("rev1");
    let events = read(out.path(), "shop.events.tsv");
    let rows = read(out.path(), "shop.rows.tsv");

    // Two reads of `title` in one method: two rows with the same kind, subject
    // and object, at different anchors.
    let reads: Vec<&str> = lines(&events)
        .into_iter()
        .filter(|l| l.starts_with("shop::Ticket::summary\t") && l.contains("\tBODY\tRead\ttitle\t"))
        .collect();
    assert_eq!(reads.len(), 2);
    assert_ne!(reads[0], reads[1]);

    // `after_commit :notify` declared twice: two events, one row.
    let declared: Vec<&str> = lines(&events)
        .into_iter()
        .filter(|l| {
            l.starts_with("shop::Comment\t")
                && l.contains("\tDECL\tCallback\tafter_commit\tnotify\t")
        })
        .collect();
    assert_eq!(declared.len(), 2);
    assert_eq!(
        count_prefix(&rows, "CALLBACK\tshop::Comment::<after_commit>\t"),
        1
    );
}

#[test]
fn the_order_lane_is_grouped_by_scope_and_never_sorted_by_value() {
    let out = run("rev1");
    let events = read(out.path(), "shop.events.tsv");
    let body = lines(&events);
    let scopes: Vec<&str> = body.iter().map(|l| l.split('\t').next().unwrap()).collect();
    let mut grouped = scopes.clone();
    grouped.dedup();
    let mut sorted = grouped.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        grouped, sorted,
        "scopes are sorted and each scope is one contiguous block"
    );

    let kinds: Vec<&str> = body
        .iter()
        .filter(|l| l.starts_with("shop::Ticket\t"))
        .map(|l| l.split('\t').nth(3).unwrap())
        .collect();
    let mut by_value = kinds.clone();
    by_value.sort_unstable();
    assert_ne!(
        kinds, by_value,
        "the Ticket declarations are in source order, not value order"
    );
    let seqs: Vec<u32> = body
        .iter()
        .filter(|l| l.starts_with("shop::Ticket\t"))
        .map(|l| l.split('\t').nth(1).unwrap().parse().unwrap())
        .collect();
    assert_eq!(seqs, (0..13).collect::<Vec<u32>>());
}

#[test]
fn config_errors_name_the_key() {
    let fail = |edit: &dyn Fn(&str) -> String| -> String {
        let (_dir, config) = variant("rev1", edit);
        Config::load(&config).unwrap_err().to_string()
    };
    let msg = fail(&|t| format!("colour = \"red\"\n{t}"));
    assert!(msg.contains("colour"), "{msg}");
    let msg = fail(&|t| t.replace("structural = true", "structural = true\nsorting = true"));
    assert!(msg.contains("sorting"), "{msg}");
    let msg = fail(&|t| t.replace("ruby-rails", "python-odoo"));
    assert!(
        msg.contains("frontend") && msg.contains("python-odoo"),
        "{msg}"
    );
    let msg = fail(&|t| t.replace("namespace = \"shop\"", "namespace = \"a b\""));
    assert!(msg.contains("namespace"), "{msg}");
    let msg = fail(&|t| t.replace("root = \"app/models\"", "root = \"../models\""));
    assert!(msg.contains("root"), "{msg}");
    let msg = fail(&|t| t.replace("root = \"app/models\"\n", ""));
    assert!(msg.contains("root"), "{msg}");
    let msg = fail(&|t| t.replace("\"imports.json\"", "\"/etc/imports.json\""));
    assert!(msg.contains("lanes.imports"), "{msg}");
    let msg = fail(&|t| t.replace("order = true", "order = \"yes\""));
    assert!(msg.contains("order"), "{msg}");
}

#[test]
fn no_cell_can_hold_a_tab_or_a_newline() {
    // A name is refused, never repaired.
    for (model, function_name) in [("A\tB", "f"), ("A", "f\ng"), ("A\rB", "f"), ("", "f")] {
        let graph = hand_model(model, vec![function(function_name, &[])]);
        assert!(
            matches!(build(&graph), Err(Error::BadCell { .. })),
            "{model:?}/{function_name:?} must be refused"
        );
    }
    // Free text is escaped.
    let mut graph = hand_model("M", vec![function("f", &["Foo.\tbar\nbaz"])]);
    graph.models[0].callbacks.push(Callback {
        phase: "before_save".to_owned(),
        target: "a\tb".to_owned(),
        options: vec![("if".to_owned(), "x\ny".to_owned())],
    });
    let built = build(&graph).unwrap();
    for row in &built.rows {
        for cell in [&row.subject, &row.object] {
            assert!(!cell.contains(['\t', '\n', '\r']), "{row:?}");
        }
    }
    assert_eq!(
        predicates(&built, "UNRESOLVED_CALL"),
        [("t::M::f".to_owned(), "Foo.\\tbar\\nbaz".to_owned())]
    );
    assert_eq!(
        predicates(&built, "CALLBACK_OPTION"),
        [(
            "t::M::<before_save>".to_owned(),
            "a\\tb:if=x\\ny".to_owned()
        )]
    );
}

#[test]
fn the_receipt_is_deterministic_and_accounts_for_the_store() {
    let out = run("rev1");
    let text = read(out.path(), "shop.receipt.json");
    let receipt: Receipt = serde_json::from_str(&text).unwrap();

    assert_eq!(receipt.direction, "ruff->store");
    assert_eq!(receipt.lanes["structural"], "present");
    assert_eq!(receipt.lanes["order"], "present");
    assert_eq!(receipt.lanes["imports"], "present");
    assert!(receipt.conservation.values().all(|b| b.rows == b.seen));
    assert_eq!(receipt.conservation["calls"].seen, 3);
    assert_eq!(receipt.cardinality.rows["CALLBACK"], 5);
    assert_eq!(receipt.cardinality.rows["IMPORTS"], 4);
    assert_eq!(receipt.cardinality.events["DECL"], 22);
    assert_eq!(receipt.cardinality.residuals["CALLEE_NOT_NAMED"], 2);
    assert_eq!(receipt.cardinality.residuals["REFERENCE_NOT_NAMED"], 2);
    let rows = read(out.path(), "shop.rows.tsv");
    let total: u32 = receipt.cardinality.rows.values().sum();
    assert_eq!(usize::try_from(total).unwrap(), lines(&rows).len());
    let events = read(out.path(), "shop.events.tsv");
    let event_total: u32 = receipt.cardinality.events.values().sum();
    assert_eq!(usize::try_from(event_total).unwrap(), lines(&events).len());

    // The snapshot digest is FNV-1a over the files.tsv lines, recomputed here.
    let files = read(out.path(), "shop.files.tsv");
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for line in lines(&files) {
        for b in format!("{line}\n").bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    assert_eq!(receipt.snapshot.digest, format!("{h:016x}"));
    assert_eq!(
        usize::try_from(receipt.snapshot.files).unwrap(),
        lines(&files).len()
    );

    // Keys come out sorted, and no absolute path leaks in.
    let top: Vec<&str> = text
        .lines()
        .filter(|l| l.starts_with("  \""))
        .map(|l| l.trim_start().split('"').nth(1).unwrap())
        .collect();
    let mut sorted = top.clone();
    sorted.sort_unstable();
    assert_eq!(top, sorted);
    assert!(!text.contains(env!("CARGO_MANIFEST_DIR")));
    assert!(!text.contains(out.path().to_str().unwrap()));
    for name in FILES {
        assert!(!read(out.path(), name).contains(env!("CARGO_MANIFEST_DIR")));
    }
}

#[test]
fn the_imports_lane_is_optional_and_read_as_data() {
    let (_tree, config) = variant("rev1", |t| t.replace("imports = \"imports.json\"\n", ""));
    let out = TempDir::new().unwrap();
    let receipt = project(&config, out.path()).unwrap();
    assert_eq!(receipt.lanes["imports"], "absent");
    assert!(receipt.imports_fnv64.is_none());
    assert!(!receipt.conservation.contains_key("imports"));
    assert_eq!(
        count_prefix(&read(out.path(), "shop.rows.tsv"), "IMPORTS\t"),
        0
    );

    let with = run("rev1");
    let rows = read(with.path(), "shop.rows.tsv");
    assert_eq!(
        count_lines(
            &rows,
            &format!(
                "IMPORTS\tshop::file:app/models/ticket.rb\tshop::file:app/models/contact.rb\t{G}\t-\t-\t-"
            )
        ),
        1
    );
    let (_tree, bad) = variant("rev1", str::to_owned);
    fs::write(bad.parent().unwrap().join("imports.json"), "[\"a.rb\"]").unwrap();
    let err = Config::load(&bad)
        .and_then(|c| Projection::build(&c).map(|_| ()))
        .unwrap_err();
    assert!(matches!(err, Error::Imports { .. }), "{err}");
}

#[test]
fn the_file_table_lists_every_ruby_file_with_its_digest() {
    let out = run("rev1");
    let files = read(out.path(), "shop.files.tsv");
    let paths: Vec<&str> = lines(&files)
        .iter()
        .map(|l| l.split('\t').next().unwrap())
        .collect();
    assert_eq!(
        paths,
        [
            "app/models/attachment.rb",
            "app/models/comment.rb",
            "app/models/contact.rb",
            "app/models/ticket.rb",
            "app/models/trackable.rb"
        ]
    );
    // rev1 and rev2 differ in `ticket.rb` alone.
    let two = run("rev2");
    let changed: Vec<String> = lines(&files)
        .into_iter()
        .zip(lines(&read(two.path(), "shop.files.tsv")))
        .filter(|(a, b)| a != b)
        .map(|(a, _)| a.split('\t').next().unwrap().to_owned())
        .collect();
    assert_eq!(changed, ["app/models/ticket.rb"]);
}
