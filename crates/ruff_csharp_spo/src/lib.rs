//! C# (Roslyn) machine-plane frontend for [`ruff_spo_triplet`].
//!
//! The actual parse runs in `harvester/` — a .NET console tool built on
//! Roslyn (`Microsoft.CodeAnalysis.CSharp`) that walks a C# corpus and
//! writes one SPO [`Triple`] per line of ndjson, in the
//! exact shape the Python/Odoo, Ruby/Rails, and C++/Tesseract frontends
//! emit. Roslyn is .NET-only, so — unlike `ruff_cpp_spo`, which drives
//! libclang from Rust — the parse step is an out-of-process tool. The seam
//! between the two halves is the ndjson contract; this crate loads it and
//! validates every predicate against the closed [`Predicate`] vocabulary so
//! a harvester bug surfaces as a hard schema error instead of silent drift.
//!
//! ```text
//! C# corpus --Roslyn harvester--> triples.ndjson --load()-->
//!     Vec<Triple> --(ruff_spo_triplet::reassemble / SPO store)--> ClassView
//! ```
//!
//! Why an out-of-process tool rather than a Rust `walk_tu` like
//! `ruff_cpp_spo`: there is no Rust-callable Roslyn. Roslyn *is* the C#
//! compiler, so it resolves base types, overrides, and member types
//! authoritatively — far better than reparsing C# with a hand-rolled
//! grammar. The cost is a process boundary; the ndjson contract keeps it
//! honest, and [`load`] is the gate — [`from_ndjson`] rejects any predicate
//! outside the closed [`Predicate`] vocabulary at parse time, so a harvester
//! bug surfaces as a hard [`ParseError`] (line + offending predicate) rather
//! than silent drift into the store.

pub use ruff_spo_triplet::{ParseError, Predicate, Triple, from_ndjson};

/// The default IRI namespace prefix every C# subject/object carries, e.g.
/// `csharp:Invoice` / `csharp:Invoice.number`. Mirrors `ruff_cpp_spo`'s
/// `cpp:` and the Odoo/Rails `odoo:` / `openproject:` prefixes; per-corpus
/// overrides go through the harvester's `--ns` flag.
pub const NAMESPACE: &str = "csharp";

/// Load harvester ndjson into triples, validating every predicate against
/// the closed [`Predicate`] vocabulary.
///
/// A thin wrapper over [`from_ndjson`] kept so callers depend on this
/// frontend's surface rather than reaching through to `ruff_spo_triplet`.
/// The validation *is* the load: `from_ndjson` rejects any non-empty line
/// that is not a well-formed [`Triple`] **and** any line whose predicate is
/// outside the closed vocabulary. An out-of-vocab predicate is a harvester
/// bug (the .NET tool emitted a string no frontend agreed on), and it
/// surfaces here as a hard [`ParseError`] naming the line and predicate —
/// never as a silently-stored triple. So a clean `Ok(_)` is itself the
/// schema guarantee; there is no separate post-load check to run.
///
/// # Errors
///
/// Returns [`ParseError`] if any non-empty line is not a valid [`Triple`],
/// or carries a predicate outside the closed [`Predicate`] vocabulary.
pub fn load(ndjson: &str) -> Result<Vec<Triple>, ParseError> {
    from_ndjson(ndjson)
}

#[cfg(test)]
mod tests {
    use super::load;

    /// The shape the Roslyn harvester emits for one C# model. This
    /// fixture exercises *every* predicate `harvester/Program.cs` can emit —
    /// `rdf:type`, `inherits_from`, `has_field`, `field_type`, `has_function`,
    /// and `is_static` — so a clean load is the standing proof that the full
    /// emitted set stays inside the closed vocabulary. If the harvester grows
    /// a new predicate, it must be added to [`super::Predicate`] first, or
    /// this load fails.
    #[test]
    fn loads_and_validates_harvester_ndjson() {
        let ndjson = concat!(
            r#"{"s":"csharp:Invoice","p":"rdf:type","o":"ogit:ObjectType","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Invoice","p":"inherits_from","o":"csharp:DbBase","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Invoice","p":"has_field","o":"csharp:Invoice.number","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Invoice.number","p":"field_type","o":"string","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Invoice","p":"has_function","o":"csharp:Invoice.Save","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Invoice.Save","p":"is_static","o":"true","f":1.0,"c":0.9}"#,
            "\n",
        );
        let triples = load(ndjson).expect("every harvester predicate is in the closed vocab");
        assert_eq!(triples.len(), 6);
        assert_eq!(triples[0].s, "csharp:Invoice");
    }

    /// The DTO-arm (body-fact) predicates + the AST-DLL signature plane —
    /// `writes_field` / `reads_field` / `raises` / `calls` / `writes_if_blank`
    /// (`EmitBodyArm`, the fuzzy-recipe-codebook fingerprint,
    /// ruff/.claude/knowledge/fuzzy-recipe-codebook.md §2) plus `returns_type`
    /// / `has_param_type` / `has_visibility` (mirroring the C++ frontend's
    /// `cpp_method`, `ruff_spo_triplet::expand.rs`). One line per predicate,
    /// shaped exactly as `harvester/Program.cs` emits it (verified against
    /// `harvester/fixtures/recipe_shapes.cs` run through the real harvester).
    /// A clean load is the standing proof the whole arm — not just the
    /// original structural scaffold — stays inside the closed vocabulary.
    #[test]
    fn loads_and_validates_body_arm_and_signature_plane_ndjson() {
        let ndjson = concat!(
            r#"{"s":"csharp:Widget.SetDefaults","p":"writes_field","o":"csharp:Widget.Name","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.SetDefaults","p":"writes_if_blank","o":"csharp:Widget.Name","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Tidy","p":"reads_field","o":"csharp:Widget.Name","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Guard","p":"raises","o":"exc:ArgumentException","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Cascade","p":"calls","o":"this.ctx.SaveChanges","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Helper","p":"returns_type","o":"int","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Helper","p":"has_param_type","o":"0:int","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Helper","p":"has_param_type","o":"1:string","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:Widget.Helper","p":"has_visibility","o":"private","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:IThing.DoThing","p":"has_visibility","o":"public","f":1.0,"c":0.9}"#,
            "\n",
        );
        let triples =
            load(ndjson).expect("every DTO-arm + signature-plane predicate is in the closed vocab");
        assert_eq!(triples.len(), 10);
        assert_eq!(triples[0].s, "csharp:Widget.SetDefaults");
    }

    /// The UI-navigation arm — the WinForms `navigates_to` Klickweg edge
    /// (`EmitNavArm`). Subject is the CLASS that navigates, object is the target
    /// screen class. Shaped exactly as the harvester emits it (verified by
    /// running the real harvester over `harvester/fixtures/nav_shapes.cs`):
    /// three via `Form.Show()`/`ShowDialog()`, one via the UserControl-SPA
    /// idiom (`HostControl` field-instantiates `ChildControl`, no `.Show()`),
    /// one via NAMESPACE-QUALIFIED hosting (`new Nested.QualifiedChild(..)` —
    /// LastSegment-normalized to the bare screen node), and one `selects_view`
    /// ribbon fact. The `SaveFileDialog` CommonDialog and the non-screen
    /// `StringBuilder` are both excluded. A clean load is the standing proof
    /// the nav arm stays inside the closed vocabulary.
    #[test]
    fn loads_and_validates_nav_arm_ndjson() {
        let ndjson = concat!(
            r#"{"s":"csharp:MainScreen","p":"navigates_to","o":"csharp:OrderScreen","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:MainScreen","p":"navigates_to","o":"csharp:SettingsScreen","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:MainScreen","p":"navigates_to","o":"csharp:CustomerScreen","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:HostControl","p":"navigates_to","o":"csharp:ChildControl","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:QualifiedHost","p":"navigates_to","o":"csharp:QualifiedChild","f":1.0,"c":0.9}"#,
            "\n",
            r#"{"s":"csharp:RibbonHost","p":"selects_view","o":"csharp:tab_reports","f":1.0,"c":0.9}"#,
            "\n",
        );
        let triples = load(ndjson).expect("navigates_to + selects_view are in the closed vocab");
        assert_eq!(triples.len(), 6);
        assert_eq!(triples[0].p, "navigates_to");
        assert_eq!(triples[0].s, "csharp:MainScreen");
        assert_eq!(triples[2].o, "csharp:CustomerScreen");
        // the UserControl-SPA edge (field-instantiation, no .Show())
        assert_eq!(triples[3].s, "csharp:HostControl");
        assert_eq!(triples[3].o, "csharp:ChildControl");
        // the namespace-qualified hosting edge: `new Nested.QualifiedChild(..)`
        // resolves to the SAME bare screen node (LastSegment normalization) —
        // without it the dominant hosting idiom in namespace-organized apps
        // harvests zero edges.
        assert_eq!(triples[4].s, "csharp:QualifiedHost");
        assert_eq!(triples[4].o, "csharp:QualifiedChild");
        // the ribbon/tab selector-assignment fact: the object is a tab FIELD,
        // not a screen, so it rides `selects_view` — `navigates_to` stays a
        // pure screen→screen graph (codex P2 on #64).
        assert_eq!(triples[5].p, "selects_view");
        assert_eq!(triples[5].s, "csharp:RibbonHost");
        assert_eq!(triples[5].o, "csharp:tab_reports");
    }

    /// A predicate the .NET tool must never emit. `load` (via `from_ndjson`)
    /// rejects it at parse time, naming the offending predicate, so the
    /// schema break is loud — a hard error, never a silently-stored triple.
    #[test]
    fn rejects_out_of_vocab_predicate() {
        let ndjson = r#"{"s":"csharp:X","p":"totally_made_up","o":"csharp:Y","f":1.0,"c":0.9}"#;
        let err = load(ndjson).expect_err("out-of-vocab predicate must fail the load");
        assert_eq!(err.line, 1);
        assert!(
            err.message.contains("totally_made_up"),
            "the error must name the offending predicate, got: {}",
            err.message
        );
    }
    /// The UI-CONFIG arm (Phase 0 labyrinth recon, the room map) —
    /// `surfaces_concept` / `handles_event` / `contains_control`
    /// (`EmitUiConfigArm` + the `--room-aliases` config binding). One line
    /// per predicate, shaped exactly as `harvester/Program.cs` emits it
    /// (verified against a real WinForms corpus: screen-classified types,
    /// Designer `+=` wiring, `Controls.Add` containment, directory→concept
    /// alias rows). The Klickweg EDGES ride the nav-arm test above; this
    /// pins the room-map half. A clean load is the standing proof the
    /// config plane stays inside the closed vocabulary.
    #[test]
    fn loads_and_validates_ui_config_arm_ndjson() {
        let ndjson = concat!(
            // room-alias concept binding (Authoritative — config-declared)
            r#"{"s":"csharp:uc_cipher_main","p":"surfaces_concept","o":"cipher_key","f":0.95,"c":0.9}"#,
            "\n",
            // Designer event wiring `this.btnSave.Click += new EventHandler(this.btnSave_Click)`
            r#"{"s":"csharp:uc_cipher_main.btnSave","p":"handles_event","o":"Click:csharp:uc_cipher_main.btnSave_Click","f":0.95,"c":0.9}"#,
            "\n",
            // Designer containment `this.panel1.Controls.Add(this.grid)`
            r#"{"s":"csharp:uc_cipher_main.panel1","p":"contains_control","o":"csharp:uc_cipher_main.grid","f":0.95,"c":0.9}"#,
            "\n",
        );
        let triples = load(ndjson).expect("every UI-config-arm predicate is in the closed vocab");
        assert_eq!(triples.len(), 3);
        assert_eq!(triples[0].o, "cipher_key");
    }

    /// The KLICKWEGE-RAIL arm — the menu quad's `part_of` (location) + `purpose`
    /// axes. Shaped exactly as `harvester/Program.cs` emits them (verified by
    /// running the real harvester over `harvester/fixtures/nav_shapes.cs`):
    /// `part_of` is the post-pass canonical menu parent (the FIRST screen that
    /// navigates to a target — walking the rail is the radix-trie menu address,
    /// so no position ordinal is stored, per the V3 LE-contract §3); `purpose`
    /// is the per-screen usability role classified from control composition
    /// (chart > grid/list > multi-input form > button/action > detail). Both
    /// ride the Inferred tier (0.85 / 0.75). A clean load is the standing proof
    /// the rail arm stays inside the closed vocabulary.
    #[test]
    fn loads_and_validates_klickwege_rail_arm_ndjson() {
        let ndjson = concat!(
            // part_of — canonical menu parent = first navigates_to opener
            r#"{"s":"csharp:OrderScreen","p":"part_of","o":"csharp:MainScreen","f":0.85,"c":0.75}"#,
            "\n",
            r#"{"s":"csharp:ChildControl","p":"part_of","o":"csharp:HostControl","f":0.85,"c":0.75}"#,
            "\n",
            r#"{"s":"csharp:QualifiedChild","p":"part_of","o":"csharp:QualifiedHost","f":0.85,"c":0.75}"#,
            "\n",
            // purpose — one per classifier branch (from control composition)
            r#"{"s":"csharp:GridScreen","p":"purpose","o":"list","f":0.85,"c":0.75}"#,
            "\n",
            r#"{"s":"csharp:ChartScreen","p":"purpose","o":"chart","f":0.85,"c":0.75}"#,
            "\n",
            r#"{"s":"csharp:FormScreen","p":"purpose","o":"form","f":0.85,"c":0.75}"#,
            "\n",
            r#"{"s":"csharp:ActionScreen","p":"purpose","o":"action","f":0.85,"c":0.75}"#,
            "\n",
            r#"{"s":"csharp:MainScreen","p":"purpose","o":"detail","f":0.85,"c":0.75}"#,
            "\n",
        );
        let triples = load(ndjson).expect("part_of + purpose are in the closed vocab");
        assert_eq!(triples.len(), 8);
        // part_of: the child screen is the subject, its canonical menu parent
        // the object (walking this rail yields the radix-trie menu address).
        assert_eq!(triples[0].p, "part_of");
        assert_eq!(triples[0].s, "csharp:OrderScreen");
        assert_eq!(triples[0].o, "csharp:MainScreen");
        // purpose: the four control-composition branches + the default.
        assert_eq!(triples[3].p, "purpose");
        assert_eq!(triples[3].o, "list");
        assert_eq!(triples[4].o, "chart");
        assert_eq!(triples[5].o, "form");
        assert_eq!(triples[6].o, "action");
        assert_eq!(triples[7].o, "detail");
    }
}
