//! `class_with_decorator` matcher: selection, emit paths, ordering, hashing.

use std::collections::BTreeMap;

use serde_json::json;

use ruff_python_dto_check::bundle::EmittedBundle;
use ruff_python_dto_check::config::Config;
use ruff_python_dto_check::matcher::function_with_decorator;
use ruff_python_dto_check::matcher::harvest_with_config;
use ruff_python_dto_check::observations::attach_observations;

const FIXTURE_PATH: &str = "models.py";

const FIXTURE: &str = r#"
from dataclasses import dataclass

@dataclass
class A(Base, metaclass=M):
    x: int
    y: str = "d"

    def m(self):
        return self.x

@dataclass
def f():
    pass

@dataclass(frozen=True)
class B:
    z: float

class C:
    c: int

@other
class D:
    d: int

class E:
    @dataclass
    class Inner:
        i: int
"#;

const CLASS_RULE: &str = r#"{
    "id": "dc",
    "kind": "class_with_decorator",
    "decorator": {"name": "dataclass"},
    "emit": {
        "name": "class.name",
        "bases": "class.bases",
        "fields": "class.fields",
        "methods": "class.methods",
        "frozen": "decorator.kwargs.frozen",
        "decorators": "class.decorators",
        "line_start": "class.range.line_start"
    }
}"#;

const FUNCTION_RULE: &str = r#"{
    "id": "fn",
    "kind": "function_with_decorator",
    "decorator": {"name": "dataclass"},
    "emit": {"name": "def.name"}
}"#;

fn config(rules: &[&str]) -> Config {
    let json = format!(r#"{{"match": [{}]}}"#, rules.join(","));
    Config::from_json_str(&json).expect("config parses")
}

fn names(bundles: &[EmittedBundle]) -> Vec<&str> {
    bundles.iter().map(|b| b.function_name.as_str()).collect()
}

#[test]
fn class_rule_matches_only_top_level_decorated_classes() {
    let cfg = config(&[CLASS_RULE]);
    let bundles = harvest_with_config(FIXTURE_PATH, FIXTURE, &cfg);
    assert_eq!(names(&bundles), vec!["A", "B"]);
}

#[test]
fn class_emit_paths_evaluate_exactly() {
    let cfg = config(&[CLASS_RULE]);
    let bundles = harvest_with_config(FIXTURE_PATH, FIXTURE, &cfg);
    let a = &bundles[0];
    let b = &bundles[1];

    assert_eq!(a.fields["name"], json!("A"));
    assert_eq!(a.fields["bases"], json!(["Base", "metaclass=M"]));
    assert_eq!(
        a.fields["fields"],
        json!([
            {"name": "x", "annotation": "int", "default": null},
            {"name": "y", "annotation": "str", "default": "\"d\""}
        ])
    );
    assert_eq!(a.fields["methods"], json!(["m"]));
    assert_eq!(a.fields["decorators"], json!(["@dataclass"]));
    assert_eq!(a.fields["frozen"], json!(null));
    assert_eq!(a.family, "models");
    assert_eq!(a.match_id, "dc");

    assert_eq!(b.fields["frozen"], json!(true));
    assert_eq!(b.fields["bases"], json!([]));
    assert_eq!(b.fields["decorators"], json!(["@dataclass(frozen=True)"]));
    assert_eq!(
        b.fields["fields"],
        json!([{"name": "z", "annotation": "float", "default": null}])
    );
    assert_eq!(b.fields["methods"], json!([]));
}

#[test]
fn function_rule_does_not_match_classes() {
    let cfg = config(&[FUNCTION_RULE]);
    let bundles = harvest_with_config(FIXTURE_PATH, FIXTURE, &cfg);
    assert_eq!(names(&bundles), vec!["f"]);

    let direct = function_with_decorator::harvest_module_with_config(FIXTURE_PATH, FIXTURE, &cfg);
    assert_eq!(names(&direct), vec!["f"]);
}

#[test]
fn mixed_rules_are_merged_in_source_order() {
    let cfg = config(&[FUNCTION_RULE, CLASS_RULE]);
    let bundles = harvest_with_config(FIXTURE_PATH, FIXTURE, &cfg);
    assert_eq!(names(&bundles), vec!["A", "f", "B"]);
    assert_eq!(
        bundles
            .iter()
            .map(|b| b.match_id.as_str())
            .collect::<Vec<_>>(),
        vec!["dc", "fn", "dc"]
    );
}

#[test]
fn unknown_kind_error_names_both_kinds() {
    let err = Config::from_json_str(r#"{"match": [{"id": "x", "kind": "nope", "emit": {}}]}"#)
        .expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("function_with_decorator"), "got: {msg}");
    assert!(msg.contains("class_with_decorator"), "got: {msg}");
}

#[test]
fn class_ast_hashes_are_distinct_and_non_empty() {
    const EMPTY_SHA256: &str =
        "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    let cfg = config(&[CLASS_RULE]);
    let bundles = harvest_with_config(FIXTURE_PATH, FIXTURE, &cfg);
    let mut family_map: BTreeMap<String, Vec<EmittedBundle>> = BTreeMap::new();
    for b in bundles {
        family_map.entry(b.family.clone()).or_default().push(b);
    }
    let mut source_map: BTreeMap<String, String> = BTreeMap::new();
    source_map.insert(FIXTURE_PATH.to_string(), FIXTURE.to_string());
    attach_observations(&mut family_map, &source_map);

    let family = &family_map["models"];
    let hash_of = |name: &str| -> String {
        family
            .iter()
            .find(|b| b.function_name == name)
            .and_then(|b| b.comparison_within_family.as_ref())
            .map(|c| c.ast_hash_self.clone())
            .expect("hash attached")
    };
    let (a, b) = (hash_of("A"), hash_of("B"));
    assert_ne!(a, b);
    assert_ne!(a, EMPTY_SHA256);
    assert_ne!(b, EMPTY_SHA256);
}
