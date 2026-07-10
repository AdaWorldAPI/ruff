//! The furnace exam (transcode-doctrine Phase 6, ruff side): prove that a
//! God-object harvest re-derives its domain concepts THROUGH the pipeline —
//! `ndjson → reassemble → concept_split (convention config) → codebook
//! check` — with every expected concept bound nonzero and every unresolved
//! method landing on the slag ledger instead of silently dropping.
//!
//! Corpus-agnostic by construction: the corpus, the convention table, the
//! codebook rows, and the expected-concept list all arrive as runtime
//! config (data-as-config) — no corpus tokens live in this file, and no
//! corpus data is ever committed. Green means a previously hand-authored
//! domain table was only an unautomated config read.
//!
//! Run:
//! ```sh
//! cargo run -p ruff_spo_triplet --example rekey_exam -- <harvest.ndjson> <exam.conf>
//! ```
//!
//! Config format (one directive per line; `#` comments):
//! ```text
//! verb=add:create        # method-name verb token -> canonical verb
//! scope=pf               # scope token stripped after the verb
//! alias=ciphers:cipher_key     # residue -> canonical concept
//! codebook=cipher_key:0x0B01   # concept -> classid (the oracle rows)
//! expect=cipher_key      # concept that MUST bind for the exam to pass
//! surface=grid:grid      # surface token -> kind (config-as-schema plane;
//!                        # kinds: enum_source / template_source / subtab /
//!                        # grid / localization). Methods matching a surface
//!                        # row are classified OUT of the concept plane
//!                        # before residue accounting (doctrine Phase 3).
//! grammar_strip=form     # structured-name grammar (doctrine Phase 5):
//! grammar_marker=f       # leading tokens to strip / the numbered-path
//! grammar_tier=form      # marker / tier names outermost-first. Residues
//! grammar_tier=section   # that parse land on the PROTOCOL plane
//!                        # (part_of tree nodes), not the unbound ledger.
//! ```

#![expect(
    clippy::print_stdout,
    reason = "the whole point of this example is to print the exam report"
)]

use std::collections::BTreeMap;

use ruff_spo_triplet::{
    ConceptConvention, Model, ModelGraph, NameGrammar, SurfaceConvention, SurfaceKind,
    check_model_graph, classify_surface, from_ndjson, parse_structured_name,
    reassemble_model_graph, rekey_model,
};

struct ExamConfig {
    convention: ConceptConvention,
    surfaces: SurfaceConvention,
    grammar: NameGrammar,
    codebook: Vec<(String, u16)>,
    expect: Vec<String>,
}

fn parse_config(text: &str) -> ExamConfig {
    let mut convention = ConceptConvention::default();
    let mut surfaces = SurfaceConvention::default();
    let mut grammar = NameGrammar::default();
    let mut codebook = Vec::new();
    let mut expect = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "verb" => {
                if let Some((tok, canon)) = value.split_once(':') {
                    convention
                        .verbs
                        .push((tok.trim().to_string(), canon.trim().to_string()));
                }
            }
            "scope" => convention.scopes.push(value.trim().to_string()),
            "alias" => {
                if let Some((from, to)) = value.split_once(':') {
                    convention
                        .concept_aliases
                        .push((from.trim().to_string(), to.trim().to_string()));
                }
            }
            "codebook" => {
                if let Some((name, id)) = value.split_once(':') {
                    let id = id.trim().trim_start_matches("0x");
                    if let Ok(id) = u16::from_str_radix(id, 16) {
                        codebook.push((name.trim().to_string(), id));
                    }
                }
            }
            "expect" => expect.push(value.trim().to_string()),
            "surface" => {
                if let Some((tok, kind)) = value.split_once(':')
                    && let Some(kind) = SurfaceKind::from_config_token(kind.trim())
                {
                    surfaces.surfaces.push((tok.trim().to_string(), kind));
                }
            }
            "grammar_strip" => grammar.strip_prefixes.push(value.trim().to_string()),
            "grammar_marker" => grammar.marker = value.trim().to_string(),
            "grammar_tier" => grammar.tier_names.push(value.trim().to_string()),
            _ => {}
        }
    }
    ExamConfig {
        convention,
        surfaces,
        grammar,
        codebook,
        expect,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(ndjson_path), Some(conf_path)) = (args.next(), args.next()) else {
        eprintln!("usage: rekey_exam <harvest.ndjson> <exam.conf>");
        std::process::exit(2);
    };
    let ndjson = std::fs::read_to_string(&ndjson_path).expect("read harvest ndjson");
    let conf = parse_config(&std::fs::read_to_string(&conf_path).expect("read exam config"));

    let triples = from_ndjson(&ndjson).expect("harvest validates against the closed vocab");
    // Derive the corpus namespace from the first class anchor (same rule
    // `reassemble` itself uses), so the exam needs no ns config.
    let namespace = triples
        .iter()
        .find(|t| t.p == "rdf:type" && t.o == "ogit:ObjectType")
        .and_then(|t| t.s.split_once(':'))
        .map_or_else(String::new, |(ns, _)| ns.to_string());
    let graph = reassemble_model_graph(&triples, &namespace);

    // Re-key every model; accumulate concept -> method count, plus the slag.
    // Schema surfaces (doctrine Phase 3, config-as-schema) are pulled OUT of
    // the concept plane FIRST: a method matching a `surface=` row is config
    // plumbing wearing a method's clothes (grid autosize, localization pass,
    // enum/template getters), never a domain action.
    let verb_tokens: Vec<String> = conf
        .convention
        .verbs
        .iter()
        .map(|(tok, _)| tok.clone())
        .collect();
    let mut concept_methods: BTreeMap<String, usize> = BTreeMap::new();
    let mut keyed_total = 0usize;
    let mut residual_total = 0usize;
    let mut residue_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut surface_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut surface_total = 0usize;
    // Protocol plane (doctrine Phase 5): un-aliased residues whose spelling
    // parses against the structured-name grammar are name-embedded tree
    // addresses (CRF form/section coordinates) — part_of nodes, never
    // unbound concepts.
    let grammar_armed = !conf.grammar.marker.is_empty() || !conf.grammar.tier_names.is_empty();
    let mut protocol_histogram: BTreeMap<String, usize> = BTreeMap::new();
    let mut protocol_total = 0usize;
    for model in &graph.models {
        let outcome = rekey_model(model, &conf.convention);
        keyed_total += outcome.keyed.len();
        residual_total += outcome.residuals.len();
        for (name, split) in &outcome.keyed {
            if let Some(surface) = classify_surface(name, &verb_tokens, &conf.surfaces) {
                surface_total += 1;
                *surface_histogram
                    .entry(format!("{:?}/{}", surface.kind, surface.surface))
                    .or_default() += 1;
                continue;
            }
            if grammar_armed
                && !split.aliased
                && let Some(parsed) = parse_structured_name(&split.concept, &conf.grammar)
            {
                protocol_total += 1;
                let mut path = String::new();
                for (tier, n) in &parsed.tiers {
                    if !path.is_empty() {
                        path.push('/');
                    }
                    path.push_str(&format!("{tier}_{n}"));
                }
                if !parsed.residue.is_empty() {
                    path.push_str(&format!(" (+{})", parsed.residue.join("_")));
                }
                *protocol_histogram.entry(path).or_default() += 1;
                continue;
            }
            *concept_methods.entry(split.concept.clone()).or_default() += 1;
            if !split.aliased {
                *residue_histogram.entry(split.concept.clone()).or_default() += 1;
            }
        }
    }

    // Bind the re-keyed concepts against the codebook oracle rows via the
    // SAME check the codebook-DTO seam uses (Boundary-4: one fold).
    let mut concept_graph = ModelGraph::new("exam");
    for concept in concept_methods.keys() {
        concept_graph.models.push(Model::new(concept.clone()));
    }
    let rows: Vec<(&str, u16)> = conf
        .codebook
        .iter()
        .map(|(n, id)| (n.as_str(), *id))
        .collect();
    let check = check_model_graph(&concept_graph, &rows);

    println!("=== furnace exam ===");
    println!(
        "models: {}   methods keyed: {keyed_total}   slag ledger: {residual_total}",
        graph.models.len()
    );
    if surface_total > 0 {
        println!("schema surfaces (config-as-schema plane): {surface_total} methods");
        let mut surfaces: Vec<(&String, &usize)> = surface_histogram.iter().collect();
        surfaces.sort_by(|a, b| b.1.cmp(a.1));
        for (surface, n) in surfaces {
            println!("  {n:5}  {surface}");
        }
    }
    if protocol_total > 0 {
        println!("protocol nodes (structured-name plane, part_of tree): {protocol_total} methods");
        let mut nodes: Vec<(&String, &usize)> = protocol_histogram.iter().collect();
        nodes.sort_by(|a, b| b.1.cmp(a.1));
        for (node, n) in nodes {
            println!("  {n:5}  {node}");
        }
    }
    println!("concepts bound ({}):", check.bound.len());
    for b in &check.bound {
        println!(
            "  {}  -> 0x{:04X}   ({} methods)",
            b.concept,
            b.class_id,
            concept_methods.get(&b.concept).copied().unwrap_or(0)
        );
    }
    println!("unbound concept residues (next config facts), top 15:");
    let mut unbound: Vec<(&String, &usize)> = residue_histogram
        .iter()
        .filter(|(c, _)| !check.bound.iter().any(|b| &b.concept == *c))
        .collect();
    unbound.sort_by(|a, b| b.1.cmp(a.1));
    for (concept, n) in unbound.into_iter().take(15) {
        println!("  {n:5}  {concept}");
    }

    // The exam gate: every expected concept bound, nonzero.
    let mut failed = false;
    for want in &conf.expect {
        match check.bound.iter().find(|b| &b.concept == want) {
            Some(b) if b.class_id != 0 => {}
            _ => {
                println!("EXAM FAIL: expected concept `{want}` did not bind nonzero");
                failed = true;
            }
        }
    }
    if failed {
        std::process::exit(1);
    }
    println!(
        "EXAM PASS: all {} expected concepts re-derived from the harvest",
        conf.expect.len()
    );
}
