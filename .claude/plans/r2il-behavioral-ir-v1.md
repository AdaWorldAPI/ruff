# R2IL Behavioral IR — v1 (Phase Zero audit + wave plan)

> **Status:** ACTIVE. Branch `claude/ruff-r2il-lancegraph-3tdt8d` (all three repos).
> **Governing rule:** Preserve semantics once. Decompose by concern. Handle
> cardinality separately. Project many ways. Never stringify typed IR merely so
> another component can rediscover the types.
> **Board pattern:** this file is the plan-of-record (lance-graph
> `.claude/plans/` convention). The orchestrating main thread is the SOLE
> writer of this file and any shared board file; workers leave records only in
> their own tag files. Board entries land in the same commit as the deliverable.

## Phase Zero audit — findings (2026-08-17, three parallel audits + probes)

### Ruff

1. **Frontend inventory.** Python (`ruff_python_ast/parser/semantic/codegen` —
   native, richest; plus `ruff_python_spo` Odoo harvest, `ruff_sqlalchemy_spo`,
   `ruff_python_dto_check` route contracts), C# (`ruff_csharp_spo`, out-of-process
   Roslyn → NDJSON contract), C++ (`ruff_cpp_spo` libclang walker +
   `ruff_cpp_codegen`), Ruby (`ruff_ruby_spo`). All `*_spo` frontends fill ONE
   shared typed IR — `ruff_spo_triplet::ir::ModelGraph` (schema-locked top level,
   per-language sibling `Vec`s on `Model`) — and `expand()`
   (`ruff_spo_triplet/src/expand.rs:109`) is the SINGLE deliberate collapse
   point to `Vec<Triple>`.
2. **RouteContract** (`ruff_python_dto_check/src/contract.rs:67-108`): concern
   groups `id / inputs / data / output / guards / provenance`, EMERGENT
   `HandlerKind` classifier, 5 cross-layer lints failing loud on dropped facts.
   Config-driven, no framework idioms hardcoded. This is the concern-decomposition
   philosophy to apply to R2IL — the philosophy, not the type.
3. **Overflow.** `ruff_spo_address::mint_factored` (`lib.rs:447`): base-255
   positional cascade (`b255_width`), `MAX_SIBLINGS_PER_TIER = 255` explicitly a
   DESIGN-SMELL lint (`soc.rs:7-8`), never a storage ceiling. Its 16-byte `Facet`
   is layout-identical to `lance_graph_contract::facet::FacetCascade`.
   **Reuse the principle** (overflow = evidence a route needs factoring), not a
   copied constant.
4. **NDJSON** is canonical ONLY for the SPO layer (`Triple` mirrors lance-graph's
   `OntologyTriple` field-for-field; `deny_unknown_fields`; closed `Predicate`
   enum ~34 variants — "frontends MUST NOT emit raw predicate strings").
5. **Native-dep wiring precedent:** `ruff_cpp_spo` — non-default feature
   (`libclang = ["dep:clang"]`), `required-features` on examples, "workspace
   builds with zero system deps by default."

### r2sleigh (checkout `/home/user/r2sleigh` @ 60942f6)

6. **`r2il` is cleanly standalone**: pure Rust, deps serde/postcard/thiserror,
   compiles in 8.25 s, everything re-exported at crate root. `R2ILOp` = one flat
   enum, 60+ variants, NAMED `Varnode` fields (`IntAdd{dst,a,b}`); variadic only
   where the semantics are variadic (`CallOther{output:Option, userop:u32,
   inputs:Vec}`, `Multiequal{dst, inputs:Vec}`); atomics carry
   `ordering: MemoryOrdering`. `Varnode{space, offset:u64, size:u32,
   meta:Option<VarnodeMetadata>}` — meta excluded from Eq/Hash (advisory).
   `SpaceId{Ram,Register,Unique,Const,Custom(u32)}`.
7. **`r2ssa` is directly consumable**: builds in 43 s INCLUDING the transitive
   libsla-sys native compile (verified in-env, exit 0). Its function-level API
   (`SSAFunction::from_blocks(&[R2ILBlock], Option<&ArchSpec>)`) needs NO
   Disassembler; `Disassembler` appears ONLY in `block.rs` (legacy single-block
   `to_ssa()` register-naming convenience) — the sole reason for the hard
   `r2sleigh-lift` dep. Feature-gating it upstream is a genuinely generic
   improvement (any SSA-only consumer sheds the native dep) but is NOT required:
   **direct consumption already works** (§17 success outcome).
8. **r2sleigh already grew the concern decomposition** (`r2ssa/src/semantic.rs`,
   `graph.rs`, `interproc.rs`). The correspondence, VERIFIED against source:

   | r2sleigh type | route |
   |---|---|
   | `SSAFunction` + `BlockTerminator` + `CFGEdge` | CONTROL |
   | `SsaGraph` (`ValueId/InstId/BlockId(u32)`, `def_of`, `uses_of`, `op_inst_by_site`) | VALUES + DEF/USE + PROVENANCE |
   | `ObjectModel` (`StackSlot/FrameObject/Global/HeapAlloc/EscapedUnknown`) | OBJECTS / ALIAS |
   | `MemorySSAFacts` (`MemoryVersion`, uses/defs by inst, memory phis) | MEMORY |
   | `PredicateFacts` (`CompareProvenance`, `BlockAssumption`, switches) | PREDICATES / GUARDS |
   | `CallSiteFacts` (`target`, `direct_target`, `CallMemoryEffect`) | CALLS |
   | `FunctionSemanticSummary` (interproc arg/memory/return effects) | SUMMARY (derived) |

   `SsaGraph` is ALREADY SoA-shaped (parallel `Vec`s indexed by u32 IDs) and
   ALREADY uses the descriptor/routed operand encoding
   (`GraphInst{inputs:Vec<ValueId>, output:Option<ValueId>}`).
   **Consequence: Ruff does not invent a behavioral ontology. It names
   r2sleigh's own decomposition as routes.**
9. **Known string-forest**: `SSAVar{name:String, version, size}` (documented
   upstream limitation) and `ObjectKind::Global{space:String,..}` — this is the
   DTO/codebook plane's work, Ruff-side.
10. **Corpus mechanics**: no `.sla` in-repo; specs come from the registry crate
    `sleigh-config` (features per arch, e2e uses `x86`). Compiled test binaries
    exist (`tests/e2e/stress_test`, `stress_test_opt`). `/home/user/ghidra` has
    `.slaspec` sources if ever needed.

### lance-graph V3

11. **Physical ABI**: `NodeRow` 512 B = `NodeGuid`(16) + `EdgeBlock`(16) +
    value(480), const-asserted; `GUIDS_PER_NODE = 32` — "32 × 16-byte GUID
    slots", Tetris-across-slots doctrine (`canonical_node.rs:793-810`).
    `FacetCascade{facet_classid:u32, tiers:[FacetTier{lo:u8,hi:u8};6]}` = 16 B;
    `CascadeShape::{G6D2,G4D3,G3D4}`. Grammar dispatch = classid → ClassView
    (slot purity: labels/positions NEVER from payload). `ENVELOPE_LAYOUT_VERSION=2`.
12. **The precedent**: `lance_graph_contract::network` sinks Tesseract's 27-class
    C++ `Network` hierarchy onto ONE FacetCascade per node —
    `facet_classid = compose_classid(NETWORK_LAYER=0x0804, ntype as u16)`
    (container concept on canon-high, subclass ordinal on custom-low, "container
    kinds, not content" mint discipline), `G6D2` payload, names/weight-blobs
    OUT-OF-LINE (Lance table keyed by classid+identity). Zero physical changes.
13. **Overflow mechanisms that exist**: (a) Tetris-across-slots in-row; (b)
    out-of-line Lance-table escape (proven: network weights, 4M-vertex FMA
    mesh); (c) designed-not-built stream-window escape. A CFG-shaped
    "many small typed rows keyed to one parent" consumer is new WIRING over
    proven pattern (b), not a new primitive.
14. **Codebook plane**: `ogar_codebook::{compose_classid, canonical_concept_id}`
    — name→u16 vocabulary registry, compile-time-drift-checked
    (`network_layer_const_matches_codebook` pattern). classid capacity nowhere
    near exhausted.
15. **Verdict: V3 is sufficient. No V4. The hypothesis stands un-falsified**
    pending the corpus measurements (which gate only the per-route LAYOUT
    choices, not the physical grammar).
16. **Honesty note**: network.rs's "byte-parity vs real Tesseract" is
    designed-for (oracle named) but in-repo tests are synthetic round-trips
    against pre-registered values. R2IL routes should meet the same standard
    they claim — no overclaiming.

## PIVOT (operator, 2026-08-18): R2IL is an INTAKE ARM, not a bypass

Typed input does not mean bypassing the furnace. R2IL enters through the same
intake-arm → ore → furnace → slag → proposer discipline every other foreign
representation uses. Slag is evidence, not failure; never hidden in `Other`.

### Delta audit — where the machinery actually lives (measured 2026-08-18)

**`lance-graph-arm-discovery` is a FALSE FRIEND — "ARM" = Association Rule
Mining** (Aerial+ transcode, arXiv 2504.19354), not "intake arm". Audited in
full: it provides NONE of foreign-shape discovery / arm generation / DTO
generation / codebook generation / contract generation / target codegen /
residual clustering / schema proposal. It consumes a DECLARED
`FeatureSpec` + discretised `Dataset` (panics on arity mismatch) and emits
`CandidateRule` → `TruthU8` → SPO ndjson. Workspace-EXCLUDED, dormant, blocked
on D-ARM-7 (Jirak floor) and D-ARM-SYN-1 (`ruff_spo_triplet::from_ndjson`
REJECTS its `implies` predicate). Board banks the finding
"arm-discovery-is-a-proposer-not-the-SPO-AST"
(`PR_ARC_INVENTORY.md:1915`) and explicitly rules out clustering reuse
(`:1908`). **Verdict: do NOT reuse for intake. Possible far-downstream
analytics reuse only** (mining correlations ACROSS already-ingested R2IL
facts), itself blocked.

**The real furnace machinery is in `ruff_spo_triplet`** — three shipped,
language-agnostic, data-as-config stages, each with an explicit residual
ledger:

| module | stage | slag mechanism |
|---|---|---|
| `concept_split.rs` | Phase 1 re-key (fix CONCEPT=CLASS) | `ResidualMethod` rows + reason; "the residual is not waste: it is the empirical boundary of the current convention" |
| `surface_schema.rs` | Phase 3 config-as-schema (pull config-wearing-method-clothes OUT before action lifting) | concept/facet residue deferred to concept_split |
| `recipe.rs` | recipe centroid classifier over fact-sets ONLY (no language tokens) | `Compensate`/`WriteRaise` = essential residue, hand-ported |

`concept_split` ships **zero domain vocabulary** — the `ConceptConvention`
(verbs, scope qualifiers, aliases) is caller-supplied. This is the seam R2IL's
architecture-specific semantics enter through: an `R2ilConvention` (userop
table, custom-space table, arch profile) is the exact analog. `recipe.rs`
states the arm contract outright: "a frontend adds the arm purely by
populating those `Vec`s from its own AST; this module runs unchanged."

### The transcode evidence — `.claude/harvest/` is the arm's artifact contract

Two worked, measured transcodes establish what an intake arm MUST produce.
Not prose — committed data.

**MedCare-rs** (`AdaWorldAPI/MedCare` C#/WinForms → Rust):
- `medcare-2.0-spo-triples.ndjson.gz` — 108,548 triples, per-predicate census
  in the README, provenance pinned (corpus `429b577`, harvester `562964f`,
  EXACT invocation flags saved so the next session doesn't re-derive them).
- `medcare-soc-split.config.json` — the SoC proposal ledger: per-class
  `{fingerprint: fnv1a:…, members, data, funcs, verdict, branches[]}`;
  `MainForm` = 330 members / 210 data / 120 funcs → `duplication_and_conflation`,
  split into named branches. **This is route decomposition as committed data.**
- `TRIAGE-RESULT.md` — the furnace measurement with a **PRE-REGISTERED bar**
  ("recoverable ≥85% PASS, <50% KILL" stated BEFORE the run), full histogram,
  the 5-method essential residue named individually, and an explicit
  "do not over-claim" caveat section.
- `compiled/medcare-actiondefs.json.gz`, `generated/do_adapters.rs`.

**openproject-nexgen-rs** (Rails → Rust):
- `2026-07-06-transpile-ledger.md` — reproducible chain
  (`ruff_ruby_spo::extract_app_with_schema → ogar_from_ruff::mint::compile_graph_ruby
  → ogar_render_askama::render_class_with_methods → committed generated Rust`),
  one repro command, counts table (945 extracted / 18 curated / **16 emitted, 2
  dropped WITH NAMED REASONS**), recipe census (98.4% recoverable,
  essential_residue = 1), classid scheme, DoD checklist with ⏳ items honest.
- `orm-ar-backprojection.toml` — data-not-code resolver config with
  `validation_states = [unmeasured|confirmed|corrected|retired]`, ALL rules
  starting `unmeasured` and a meta key `measure_dont_claim`.
- `c4-rename-seed.ndjson` — vocabulary drift table with an explicit
  `identity-default` catch-all ROW (a declared rule, not a silent fallback).

**Consequence for PR 1 — the arm's deliverable is an artifact set, not a
struct.** `ruff_r2il` must emit, into `.claude/harvest/`:
1. an **ore file** (typed, lossless, deterministic) + provenance block
   (corpus, r2sleigh commit, arch, EXACT invocation);
2. a **census** (per-opcode/per-fact counts — the `108,548 triples by
   predicate` analog);
3. a **residual ledger** (slag rows with a deterministic shape id + reason,
   grouped and counted — the `ResidualMethod` analog);
4. a **conservation line**: `harvested N / classified X / residual Y /
   dropped 0` — dropped MUST be 0, Y MUST NOT be driven to 0 by a catch-all;
5. a **pre-registered bar** stated BEFORE the first run.

## OPERATOR RULING (2026-08-18): the V3-shaped varnode is the DRILL KEY

The prior intakes' method — data-as-config where drilling down produces the
nested config that in turn gives the drilling its structure ("what to bolt
where") — had ad-hoc nesting keys (class names, method-name conventions).
For R2IL the **VarnodeFacet provides the shape for the drilling**:

- `VarnodeFacet` = the 16-byte V3-shaped identity
  `classid(space-class) | offset_lo | offset_hi | size` — prefix-routable by
  construction.
- `R2ilConvention` is therefore NOT a flat table: it is a
  **longest-prefix-wins config tree over varnode identity space** (space
  class → offset(-range) → size), the same resolution rule as OGAR's
  codebook scoping ("longest-prefix wins — one rule, every level"). Rendered
  as nested TOML/JSON like the harvest precedents.
- **Slag rows are addressed**: each residual carries the facet coordinate
  where it occurred; the proposer emits proposed config rows AT those
  addresses; pass N+1 drills with them. The config accumulates as a radix
  tree, self-scaffolding.
- **Bootstrap is read, not typed**: `ArchSpec`'s register table
  (`add_register(name, offset, size)`) already IS facet-address → name rows —
  the Register-space branch of the convention is populated from upstream data
  (data-as-config doctrine: data that exists must be READ).
- Scope guard: this promotes the facet as the ADDRESS/CONFIG-KEY shape now;
  actual V3 SoA persistence stays stage 5 / PR 2. Same 16 bytes, two roles,
  no storage-layout commitment yet. The `SpaceId::Custom(u32)` lossless-fit
  falsifier becomes MORE central (config keys must be lossless).

**Why the furnace, in one line (operator, 2026-08-18): "Varnode in the first
stage is pointer chasing stacked god objects — hence the ore furnace slag."**
The upstream typed truth is GOOD ORE but structurally still stage-1 pointer
chasing: `SSAFunction`'s private `HashMap<u64, SSABlock>`, a petgraph CFG,
`BTreeMap<SSAVar, ValueId>` keyed by String-carrying vars, facts as nested
BTreeMaps of structs. **Typed ≠ refined.** The arm preserves that truth
untouched (never flattens at intake); the FURNACE is what melts the object
graph into flat facet-addressed concern rows; the SLAG is what resisted
flattening. Do not mistake the cleanliness of r2il's Rust types for
refinement — that mistake is exactly the "privileged direct path" the pivot
forbids.

## CARRIED FORWARD (2026-08-18) — the ruff/r2sleigh half of the console ruling

The operator's Ghidra-console ruling belongs to a different session; the
console, the Java extension, and the forensic product families are **out of
scope here**. Four of its requirements are pure ruff/r2sleigh R2IL properties
and DO carry momentum into PR 1. Recorded so they are not re-derived:

**C1 — the library seam must survive an external caller.** Falsifiable test:
*could a thin external caller analyze one function and obtain structured
ore/furnace/slag results without parsing CLI strings or NDJSON?* **Measured
YES** — every entry point is a typed Rust fn over library types
(`FunctionBehavior::from_blocks_raw`, `furnace::smelt`, `ResidualLedger::*`,
`HarvestReport::*`); the CLI/TSV/TOML surfaces exist ONLY inside the two
`lift`-gated examples, whose artifacts are declared *evidence, never a
re-ingest path*. **Do not seal the API around CLI-only assumptions** — no RPC
protocol now, just keep the typed seam public.

**C2 — provenance must reach the native instruction, not just the block.**
`FactProvenance.op_site: (block_addr, op_idx)` →
`R2ILBlock::op_metadata[op_idx].instruction_addr: Option<u64>`
(r2il `metadata.rs:103`: *"Source instruction address for this operation when
lifted as part of a block"*). **SSA does not carry it**, so this sidecar
rejoin IS the anchor — the same `(block_addr, op_idx)` key
`SsaGraph::op_inst_by_site` uses. Landed as `ore::instruction_addr(prov,
blocks)` so a caller finds a named API instead of rediscovering a convention.
Chain: `fact → concern route → SSA/R2IL fact → instruction address → artifact`.

**C3 — the conservation ledger is load-bearing, not diagnostic decoration.**
Three readings of one number: *what did the target fail to represent*
(transcode) · *what have we not yet explained* (reconstruction) · *what still
needs attention* (any investigative use). `dropped == 0` is what makes it
evidence rather than a progress bar. Already the PR-1 invariant; this
elevates it from QA metric to product property.

**C4 — transcode is TARGET-ARCHITECTURE PROJECTION, not syntax conversion.**
`behavioral truth → target recipe → generated implementation`. A recovered
pointer graph does NOT oblige a pointer graph in the target; the furnace emits
concern-separated facts and a target profile decides the layout. PR 1 contains
no codegen, and `FlatFact` rows are concern-tagged + architecture-neutral, so
nothing here binds a future emitter. Corollary for the eventual roundtrip
oracle: **success is semantic/behavioral parity, never textual or binary
equality** — which is exactly why §14's reconstruction oracle is specified as
`R2IL → routes → semantic-equivalent R2IL`, with SPO explicitly NOT the oracle.

## Architecture (ratified by operator feedback 2026-08-17)

```
Ruff
                     │
        ┌────────────┴─────────────┐
        │                          │
  StructuralContract       BehavioralContract      ← ruff_r2il (NEW)
        │                          │
   ModelGraph                  R2IL / SSA           (r2il + r2ssa, typed, direct)
        │                          │
     expand()                 concern routes        (r2ssa's OWN decomposition, named)
        │                          │
   Vec<Triple>              V3 SoA / overflow       (FacetCascade grammar + mint_factored
        │                          │                 principle + out-of-line escape)
        └────────────┬─────────────┘
                     │
                  OGAR/ClassView
```

- `ModelGraph → expand() → Vec<Triple>` stays what it is: ONE deliberate,
  documented-lossy SPO projection. The `Predicate` enum describes facts we
  INTENTIONALLY project to SPO; it never becomes a backdoor opcode vocabulary.
- Canonical behavioral truth = the typed r2il/r2ssa values, assembled by
  `ruff_r2il` into a `FunctionBehavior` contract whose route accessors ARE
  r2sleigh's own `SsaGraph`/`PreparedFunctionFacts` decomposition.
- DTO/ClassId plane: intern `SSAVar.name`, userop ids, space categories, arch
  names, symbols — numeric identities + codebook resolution; strings never hang
  off operational objects in the projected form.
- Forbidden and staying forbidden: R2IL→JSON→Ruff; serde_json::to_value(op);
  parsing display strings; generic edge soup; per-R2IL-struct 16-byte dogma.

## Wave plan

**PR 1 — ruff: `crates/ruff_r2il` (typed ingest + fixtures + corpus profiler).**
Workspace-EXCLUDED standalone crate (root `Cargo.toml` exclude; bgz17/deepnsm
precedent) so bare `cargo check --workspace` in ruff stays sibling-free and
`Cargo.lock` untouched. Path deps `../../../r2sleigh/crates/{r2il,r2ssa}`
(AdaWorldAPI fork, P0 fork rule). Feature `lift` (non-default, ruff_cpp_spo
pattern) gates `r2sleigh-lift` + `sleigh-config/x86` for the profiler example.
Contents:
  - `behavior.rs`: `FunctionBehavior{identity, ssa, graph, facts, summary}` +
    named route accessors (control/values/objects/memory/predicates/calls) —
    a thin truthful assembly, zero copying, no parallel ontology.
  - `vocab.rs`: deterministic vocabulary harvest (SSAVar names, userops, space
    categories) → interning table; measures the string-forest collapse.
  - `facet.rs`: `VarnodeFacet` 16-byte projection EXPERIMENT (classid=space
    class, a=offset lo32, b=offset hi32, c=size) + the `SpaceId::Custom(u32)`
    lossless falsifier (custom ids need codebook interning — measure the fit).
    Documented as a projection probe, not an address system.
  - `tests/`: §14 lossless fixtures — every mandated op (Copy, IntAdd, Load,
    Store, cmp, CBranch, Branch, Call, Return, AtomicCAS, StoreConditional,
    Load/StoreGuarded, CallOther >2 in, Multiequal >2, Insert, Custom space,
    64-bit offsets, ordering, optional outputs, metadata) through
    `FunctionBehavior::from_blocks` with typed-preservation asserts.
  - `examples/r2il_corpus_profile.rs` (feature `lift`): §12 profile over real
    corpora (r2sleigh e2e binaries + in-container ELFs): opcode freq, arity
    histograms, %fitting dst+src0+src1, blocks/fn, ops/block, phi fan-in,
    bytes/op for candidate layouts, overflow frequency at 255-rank.

**PR 2 — ruff: route→V3 projection + DTO hooks (gated on PR 1 numbers).**
Measured layout choice per §11 (inline vs descriptor vs hybrid — note SsaGraph
is already descriptor-shaped in memory), `mint_factored`-principle overflow per
route, codebook wiring (read ogar_codebook, never construct a parallel one),
optional SPO projection of SEMANTIC facts only (calls/objects), round-trip
reconstruction oracle (R2IL → routes → semantic-equivalent R2IL; SPO explicitly
NOT the oracle).

**PR 3 — lance-graph: ONLY if proven owner.** Expected residual: the codebook
mint for R2IL container concept(s) (the `NETWORK_LAYER=0x0804` analog) — a
canon-high slot is minted in `ogar_codebook`, which lance-graph owns. Defer
until PR 2 proves the route set; provisional classids documented Ruff-side
until then.

**PR 4 — r2sleigh: candidate, not required.** Feature-gate `r2sleigh-lift` in
`r2ssa` (only `block.rs` needs it) → SSA-only consumers shed the native dep.
Generic, roadmap-aligned, Ruff-free. File upstream only with operator consent;
direct consumption already succeeds without it.

## Stop conditions hit so far
- §22.1: direct r2il/r2ssa consumption solves the upstream seam — YES (43 s).
- §22.4: V3 represents R2IL via routes+overflow — YES per audit; corpus gates layout only.
- §22.5: no V4; variable arity already routed upstream (SsaGraph descriptor shape).

## Open items
- O1: corpus profile numbers (PR 1 gate for PR 2 layout).
- O2: def-use persist-vs-derive benchmark (§13) — SsaGraph derives def_of/uses_of
  from SSAFunction cheaply; measure before persisting.
- O3: `SpaceId::Custom(u32)` fit in the 16-byte varnode projection (fixture).
- O4: function discovery for whole-ELF lifting (linear sweep vs CLI vs r2) —
  profiler may start at instruction/op-level stats + e2e binaries.
- O5: classid mint request shape for lance-graph (PR 3 gate).
