//! Projection into the shared `ModelGraph`.

use ruff_spo_triplet::{Function, Model, ModelGraph};

use crate::harvest::Module;

/// One model per module and one function per rule head, in order of first
/// definition. Dots are replaced by underscores, because the expander uses the dot
/// to separate a model from its members.
#[must_use]
pub fn to_model_graph(namespace: &str, modules: &[Module]) -> ModelGraph {
    let mut graph = ModelGraph::new(namespace);
    for module in modules {
        let mut model = Model::new(module.name.replace('.', "_"));
        for rule in &module.rules {
            let Some(head) = rule.head_name() else {
                continue;
            };
            let name = head.replace('.', "_");
            if !model.functions.iter().any(|f| f.name == name) {
                model.functions.push(Function {
                    name,
                    ..Function::default()
                });
            }
        }
        graph.models.push(model);
    }
    graph
}
