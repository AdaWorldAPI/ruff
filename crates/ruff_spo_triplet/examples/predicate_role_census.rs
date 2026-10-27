//! Count the predicate roles in a harvest ndjson corpus, and measure how many
//! compute functions are candidates for lowering as pure code.
//!
//! Part 1 counts every triple once per role bit it carries
//! ([`ruff_spo_triplet::Predicate::role`]) and lists the roles no triple
//! carries. Those absent roles are the parts of the source's behaviour the
//! harvest did not capture.
//!
//! Part 2 is the purity fold. A compute function is a function whose every
//! written field is a declared computed field (a `depends_on` subject). It is
//! a pure candidate if it also raises nothing. `calls` facts decide whether a
//! candidate also dispatches nothing; when the corpus carries no `calls`
//! facts at all, the report says so and the candidates stay unverified on
//! that point. The recipe classifier is run on the same functions as an
//! independent cross-check.
//!
//! The corpus path is never hardcoded:
//! ```sh
//! cargo run -p ruff_spo_triplet --example predicate_role_census -- <ndjson-path>
//! ```

#![expect(
    clippy::print_stdout,
    reason = "the whole point of this example is to print the corpus report"
)]

use std::collections::BTreeSet;

use ruff_spo_triplet::{
    Predicate, PredicateRole, RecipeCentroid, classify, from_ndjson, group_functions, role_census,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("CORPUS_NDJSON").ok())
        .ok_or(
            "usage: predicate_role_census <ndjson-path>  (or set CORPUS_NDJSON)\n\
             the corpus path is never hardcoded — pass it explicitly",
        )?;
    let triples = from_ndjson(&std::fs::read_to_string(&path)?)?;

    let census = role_census(&triples);
    println!("corpus: {path}");
    println!("triples: {}", census.triples);
    println!("\nrole            triples");
    for ((_, name), n) in PredicateRole::NAMED.iter().zip(census.per_role) {
        println!("{name:<15} {n:>7}");
    }
    let absent: Vec<&str> = census.absent().collect();
    println!("\nroles carried by no triple: {}", absent.join(", "));
    println!("unknown predicates: {}", census.unknown.len());

    let groups = [
        ("description", PredicateRole::DESCRIPTION),
        ("capability", PredicateRole::CAPABILITY),
        ("do-arm", PredicateRole::DO_ARM),
        ("precondition", PredicateRole::PRECONDITION),
        ("guard", PredicateRole::GUARD),
        ("dataflow", PredicateRole::DATAFLOW),
    ];
    println!("\ngroup           triples (a triple counts once per group it touches)");
    for (name, group) in groups {
        let n = triples
            .iter()
            .filter_map(|t| Predicate::from_str(&t.p))
            .filter(|p| p.role().intersects(group))
            .count();
        println!("{name:<15} {n:>7}");
    }

    let count_p = |p: Predicate| triples.iter().filter(|t| t.p == p.as_str()).count();
    let calls_facts = count_p(Predicate::Calls);
    let writes_field_facts = count_p(Predicate::WritesField);

    let computed: BTreeSet<&str> = triples
        .iter()
        .filter(|t| t.p == Predicate::DependsOn.as_str())
        .map(|t| t.s.as_str())
        .collect();
    let functions = group_functions(&triples);
    let compute_fns: Vec<_> = functions
        .iter()
        .filter(|f| !f.writes.is_empty() && f.writes.iter().all(|w| computed.contains(w.as_str())))
        .collect();
    let no_raise: Vec<_> = compute_fns
        .iter()
        .copied()
        .filter(|f| f.raises.is_empty())
        .collect();
    let no_raise_no_call: Vec<_> = no_raise
        .iter()
        .copied()
        .filter(|f| f.calls.is_empty())
        .collect();
    let single_write = no_raise_no_call
        .iter()
        .filter(|f| f.writes.len() == 1)
        .count();
    let recipe_compute = no_raise_no_call
        .iter()
        .filter(|f| classify(**f) == RecipeCentroid::Compute)
        .count();

    println!("\npurity fold");
    println!("functions                       {:>7}", functions.len());
    println!("computed fields (depends_on s)  {:>7}", computed.len());
    println!("compute functions               {:>7}", compute_fns.len());
    println!("  raising nothing               {:>7}", no_raise.len());
    println!(
        "  and with no calls fact        {:>7}",
        no_raise_no_call.len()
    );
    println!("    writing exactly one field   {single_write:>7}");
    println!("    recipe centroid = Compute   {recipe_compute:>7}");
    println!("\ncoverage: calls facts {calls_facts}, writes_field facts {writes_field_facts}");
    if calls_facts == 0 {
        println!(
            "calls is NOT HARVESTED in this corpus: 'no calls fact' means unknown, not none. \
             The candidates above are unverified on dispatch."
        );
    }
    Ok(())
}
