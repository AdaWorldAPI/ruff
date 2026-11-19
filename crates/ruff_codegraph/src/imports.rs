//! The imports lane: file-level import edges, read as data.
//!
//! The input is the JSON `ruff analyze graph` prints: an object mapping a file
//! path to an array of the file paths it imports (`ruff_graph::ImportMap`, a
//! `BTreeMap<SystemPathBuf, ModuleImports(BTreeSet<SystemPathBuf>)>`
//! serialized with `serde_json`). This crate reads the JSON and does not link
//! `ruff_graph`.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Error;
use crate::rows::{Builder, Family, GRADE_STRUCTURE, Obj};

/// Distinct `(importer, imported)` pairs.
pub type Edges = BTreeSet<(String, String)>;

/// Parse the JSON text. `path` is only used to name the file in errors.
pub fn parse(path: &str, json: &str) -> Result<Edges, Error> {
    let map: BTreeMap<String, Vec<String>> =
        serde_json::from_str(json).map_err(|e| Error::Imports {
            path: path.to_owned(),
            message: format!("expected an object mapping a path to an array of paths: {e}"),
        })?;
    let mut edges = Edges::new();
    for (from, targets) in map {
        for to in targets {
            edges.insert((from.clone(), to));
        }
    }
    Ok(edges)
}

/// Add one `IMPORTS` row per edge.
pub fn project(namespace: &str, edges: &Edges, builder: &mut Builder) -> Result<(), Error> {
    for (from, to) in edges {
        let subject = format!("{namespace}::file:{from}");
        let object = format!("{namespace}::file:{to}");
        builder.put(
            "IMPORTS",
            &subject,
            Obj::Name(&object),
            GRADE_STRUCTURE,
            Family::Import,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_analyze_graph_shape() {
        let edges = parse("x", r#"{"a.rb": ["b.rb", "c.rb", "b.rb"], "b.rb": []}"#).unwrap();
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn refuses_other_shapes() {
        assert!(parse("x", r#"["a.rb"]"#).is_err());
        assert!(parse("x", r#"{"a.rb": "b.rb"}"#).is_err());
    }
}
