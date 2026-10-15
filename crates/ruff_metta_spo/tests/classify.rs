//! Behaviour of the harvest and the rule classifier, on small MeTTa programs.

use ruff_metta_spo::{
    Category, Natives, Reason, Verdict, classify, hyperon_stdlib, parse_module, to_model_graph,
};

fn hyperon() -> Natives {
    Natives::hyperon()
}

fn verdict_of(src: &str, head: &str) -> Verdict {
    let module = parse_module("t", src);
    assert!(module.parse_error.is_none(), "{:?}", module.parse_error);
    classify(&hyperon(), &[module])
        .verdicts
        .get(head)
        .cloned()
        .unwrap_or_else(|| panic!("no verdict for {head}"))
}

#[test]
fn harvest_separates_rules_types_facts_and_queries() {
    let module = parse_module(
        "kb",
        "; a comment\n\
         (parent Tom Bob)\n\
         (parent Bob Ann)\n\
         (: grandparent (-> Atom Atom))\n\
         (= (grandparent $x) (match &self (, (parent $x $y) (parent $y $z)) $z))\n\
         !(grandparent Tom)\n",
    );
    assert_eq!(module.parse_error, None);
    assert_eq!(module.facts.len(), 2);
    assert_eq!(module.types.len(), 1);
    assert_eq!(module.types[0].name, "grandparent");
    assert_eq!(module.rules.len(), 1);
    assert_eq!(module.rules[0].head_name(), Some("grandparent"));
    assert_eq!(module.queries.len(), 1);
}

#[test]
fn a_conjunctive_match_is_lowerable() {
    let v = verdict_of(
        "(= (grandparent $x) (match &self (, (parent $x $y) (parent $y $z)) $z))",
        "grandparent",
    );
    assert_eq!(v, Verdict::Lowerable);
}

#[test]
fn arithmetic_makes_a_rule_interpreter_only() {
    let v = verdict_of("(= (inc $x) (+ $x 1))", "inc");
    assert_eq!(
        v,
        Verdict::InterpreterOnly(Reason::Builtin {
            name: "+".to_string(),
            category: Category::Arithmetic,
        })
    );
}

/// The case a keyword count gets wrong: `a` names no builtin itself, but it calls
/// `b`, which does. The taint has to travel up the call graph.
#[test]
fn calling_an_interpreter_only_rule_is_interpreter_only() {
    let module = parse_module("t", "(= (a $x) (b $x))\n(= (b $x) (+ $x 1))\n");
    let c = classify(&hyperon(), &[module]);
    assert_eq!(
        c.verdicts["a"],
        Verdict::InterpreterOnly(Reason::Calls {
            callee: "b".to_string()
        })
    );
    assert!(matches!(
        c.verdicts["b"],
        Verdict::InterpreterOnly(Reason::Builtin { .. })
    ));
}

/// The taint has to travel more than one step: `a` -> `b` -> `c` -> `+`.
#[test]
fn taint_travels_the_whole_call_chain() {
    let module = parse_module(
        "t",
        "(= (a $x) (b $x))\n(= (b $x) (c $x))\n(= (c $x) (+ $x 1))\n",
    );
    let c = classify(&hyperon(), &[module]);
    for head in ["a", "b", "c"] {
        assert!(
            matches!(c.verdicts[head], Verdict::InterpreterOnly(_)),
            "{head}: {:?}",
            c.verdicts[head]
        );
    }
}

/// Recursion alone does not taint: ancestor is the textbook recursive Datalog rule.
#[test]
fn recursion_through_match_stays_lowerable() {
    let module = parse_module(
        "t",
        "(= (anc $x) (match &self (parent $x $y) $y))\n\
         (= (anc $x) (anc (match &self (parent $x $y) $y)))\n",
    );
    assert_eq!(
        classify(&hyperon(), &[module]).verdicts["anc"],
        Verdict::Lowerable
    );
}

/// A recursive cycle that touches a builtin anywhere is tainted as a whole.
#[test]
fn a_tainted_cycle_taints_every_member() {
    let module = parse_module(
        "t",
        "(= (even $x) (odd $x))\n(= (odd $x) (even (- $x 1)))\n",
    );
    let c = classify(&hyperon(), &[module]);
    assert!(matches!(c.verdicts["even"], Verdict::InterpreterOnly(_)));
    assert!(matches!(c.verdicts["odd"], Verdict::InterpreterOnly(_)));
}

/// A `match` pattern is matched, never evaluated, so a builtin name inside the
/// pattern is data. Silence twin of `arithmetic_makes_a_rule_interpreter_only`.
#[test]
fn builtin_names_inside_a_match_pattern_are_data() {
    let v = verdict_of("(= (logged $x) (match &self (println! $x) $x))", "logged");
    assert_eq!(v, Verdict::Lowerable);
}

/// The template of a `match` is evaluated after matching, so a builtin there taints.
#[test]
fn builtins_in_a_match_template_taint() {
    let v = verdict_of("(= (next $x) (match &self (n $x $y) (+ $y 1)))", "next");
    assert!(matches!(
        v,
        Verdict::InterpreterOnly(Reason::Builtin {
            category: Category::Arithmetic,
            ..
        })
    ));
}

#[test]
fn one_tainted_clause_taints_the_head() {
    let module = parse_module(
        "t",
        "(= (f $x) (match &self (p $x $y) $y))\n(= (f $x) (if $x a b))\n",
    );
    let c = classify(&hyperon(), &[module]);
    // `if` is unresolved without the prelude, so it is a constructor; check with it.
    assert_eq!(c.verdicts["f"], Verdict::Lowerable);
    let module = parse_module(
        "t",
        "(= (f $x) (match &self (p $x $y) $y))\n(= (f $x) (let $y $x $y))\n",
    );
    let c = classify(&hyperon(), &[hyperon_stdlib(), module]);
    assert!(matches!(c.verdicts["f"], Verdict::InterpreterOnly(_)));
}

/// An expression head with no rule and no builtin is a data constructor. It is
/// reported, so an unknown function is visible instead of silently "lowerable".
#[test]
fn unresolved_heads_are_reported_as_constructors() {
    let module = parse_module("t", "(= (wrap $x) (Box $x))\n");
    let c = classify(&hyperon(), &[module]);
    assert_eq!(c.verdicts["wrap"], Verdict::Lowerable);
    assert!(c.constructors.contains("Box"));
    assert!(!c.constructors.contains("wrap"));
}

#[test]
fn a_variable_headed_expression_is_conservatively_interpreter_only() {
    let v = verdict_of("(= (apply $f $x) ($f $x))", "apply");
    assert_eq!(v, Verdict::InterpreterOnly(Reason::VariableHead));
}

#[test]
fn the_hyperon_stdlib_classifies_through_its_minimal_instructions() {
    let c = classify(&hyperon(), &[hyperon_stdlib()]);
    // `let` is `unify`, a minimal-MeTTa instruction.
    assert_eq!(
        c.verdicts["let"],
        Verdict::InterpreterOnly(Reason::Builtin {
            name: "unify".to_string(),
            category: Category::Instruction,
        })
    );
    assert!(matches!(
        c.verdicts["collapse"],
        Verdict::InterpreterOnly(_)
    ));
    // `id` returns its argument and `if` is two pattern clauses.
    assert_eq!(c.verdicts["id"], Verdict::Lowerable);
    assert_eq!(c.verdicts["if"], Verdict::Lowerable);
    // Anti-vacuity: the stdlib is not classified wholesale one way.
    let lowerable = c
        .verdicts
        .values()
        .filter(|v| **v == Verdict::Lowerable)
        .count();
    assert!(
        lowerable > 0 && lowerable * 2 < c.verdicts.len(),
        "{lowerable} of {}",
        c.verdicts.len()
    );
}

#[test]
fn projection_emits_one_function_per_rule_head() {
    let module = parse_module(
        "kb",
        "(= (f $x) (g $x))\n(= (f $x) (h $x))\n(= (g $x) $x)\n",
    );
    let graph = to_model_graph("metta", &[module]);
    assert_eq!(graph.models.len(), 1);
    let names: Vec<&str> = graph.models[0]
        .functions
        .iter()
        .map(|f| f.name.as_str())
        .collect();
    assert_eq!(names, ["f", "g"]);
    let triples = ruff_spo_triplet::expand(&graph);
    assert!(
        triples
            .iter()
            .any(|t| t.p == "has_function" && t.s == "metta:kb" && t.o == "metta:kb.f")
    );
}

#[test]
fn a_parse_error_is_recorded_not_panicked() {
    let module = parse_module("bad", "(= (f $x) (g $x)\n");
    assert!(module.parse_error.is_some());
}

/// One rule, two engines. Hyperon's `if` is two pattern clauses in its stdlib;
/// PeTTa's registry lists `if` as a compiler form.
#[test]
fn the_same_rule_gets_a_verdict_per_engine() {
    let program = parse_module("t", "(= (pick $c) (if $c yes no))\n");
    let hyperon = classify(&Natives::hyperon(), &[hyperon_stdlib(), program.clone()]);
    assert_eq!(hyperon.verdicts["pick"], Verdict::Lowerable);
    let petta = classify(&Natives::petta_fork(), &[program]);
    assert_eq!(
        petta.verdicts["pick"],
        Verdict::InterpreterOnly(Reason::Builtin {
            name: "if".to_string(),
            category: Category::Instruction,
        })
    );
}
