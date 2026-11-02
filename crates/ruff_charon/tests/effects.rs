//! Witnessed effects: `<crate>.effects.tsv` records each effect the compiler's
//! LLBC shows in a function body (a built-in or user assert, with its exact
//! kind; a write through a dereferenced place). It is a list of observations
//! only: a function with no row is NOT proven pure (asserts removed by
//! Charon's own passes, callees' effects and unharvested bodies are not
//! here), so nothing may read the absence of a row as purity.
#![expect(
    clippy::disallowed_methods,
    reason = "tests write and read their own temp files; there is no ty System here"
)]

use std::path::PathBuf;

use ruff_charon::load;
use ruff_charon::store::Store;

fn data(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn import(name: &str) -> (PathBuf, String) {
    import_crate(name, "cg_lib")
}

fn import_crate(name: &str, krate: &str) -> (PathBuf, String) {
    let dir = std::env::temp_dir().join(format!("ruff_charon_fx_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let llbc = load(&data(&format!("{krate}.llbc"))).expect("llbc");
    Store::new(&dir).import(&llbc, krate).expect("import");
    let text =
        std::fs::read_to_string(dir.join(format!("{krate}.effects.tsv"))).expect("effects file");
    (dir, text)
}

/// `(effect, kind)` of every row of `subject`, in file order.
fn effects_of<'a>(text: &'a str, subject: &str) -> Vec<(&'a str, &'a str)> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[0] == subject).then(|| (f[1], f[2]))
        })
        .collect()
}

/// Fails if the file is missing or its header changes without a version bump.
#[test]
fn the_effects_file_has_a_versioned_header() {
    let (dir, text) = import("header");
    let mut lines = text.lines();
    assert_eq!(lines.next(), Some("#version 1"));
    assert_eq!(
        lines.next(),
        Some("#schema subject\teffect\tkind\tfile\tstart\tend")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The N7 witness pair. Today the relation rows of `double` (`x * 2`) and
/// `unit` are indistinguishable although Charon saw an overflow check in
/// `double`. The check must be recorded with its exact kind.
#[test]
fn a_witnessed_overflow_check_is_recorded_with_its_kind() {
    let (dir, text) = import("witness");
    assert_eq!(
        effects_of(&text, "cg_lib::util::double"),
        [("ASSERT", "Overflow:Mul")]
    );
    assert_eq!(
        effects_of(&text, "cg_lib::util::triple"),
        [("ASSERT", "Overflow:Mul")]
    );
    // `double(self.0) / 2 * self.0`: two different kinds, in body order.
    assert_eq!(
        effects_of(&text, "cg_lib::shapes::impl_Shape_for_Square::area"),
        [("ASSERT", "DivisionByZero"), ("ASSERT", "Overflow:Mul")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The silence twin: a function with no witnessed effect has no row. This is
/// the absence of an observation, never a purity verdict.
#[test]
fn a_function_without_a_witnessed_effect_has_no_row() {
    let (dir, text) = import("silent");
    assert!(effects_of(&text, "cg_lib::util::unit").is_empty());
    assert!(effects_of(&text, "cg_lib::util::private_unused").is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fails if repeated effects of one kind are collapsed: one row per
/// occurrence, so a count survives. `cg_app::run` has three additions, each
/// with its own overflow check; a kind-level dedup would leave one.
#[test]
fn every_occurrence_is_its_own_row() {
    let (dir, text) = import_crate("multi", "cg_app");
    assert_eq!(
        effects_of(&text, "cg_app::run"),
        [("ASSERT", "Overflow:Add"); 3]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Fails if the effect rows disagree with the independent reference output
/// (`cg_lib.llbc_calls.txt`, written by another tool), which counts the
/// asserts per function.
#[test]
fn the_assert_count_matches_the_reference() {
    let (dir, text) = import("reference");
    let total_asserts = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter(|l| l.split('\t').nth(1) == Some("ASSERT"))
        .count();
    let reference: usize = std::fs::read_to_string(data("cg_lib.llbc_calls.txt"))
        .expect("reference")
        .lines()
        .filter_map(|l| l.rsplit("assert=").next()?.parse::<usize>().ok())
        .sum();
    assert_eq!(total_asserts, reference);
    let _ = std::fs::remove_dir_all(&dir);
}
