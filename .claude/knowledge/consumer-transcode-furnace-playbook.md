# The Consumer-Transcode Furnace — ore/slag, two oracles, no hand-rolling

> **Type:** knowledge (methodology — teaches the *how*, portable across
> every legacy-app→Rust transcode, not one answer).
> **READ BY:** any session transcoding a legacy application into a Rust
> consumer via the `ruff_*_spo` harvest + the OGAR codebook — specifically
> **odoo → odoo-rs**, **redmine + OpenProject → openproject-nexgen-rs**,
> **WoA → woa-rs**, and the worked reference **MedCare → MedCare-rs**. Also
> read by anyone designing a new harvester arm, a new closed-vocab predicate,
> or a consumer render surface. Companion to `fuzzy-recipe-codebook.md` (that
> doc cooks method *bodies*; this doc frames the *whole loop* around it).
> **Status:** FINDING — the loop was run end-to-end on the MedCare (C#)
> corpus through 2026-07; the method is corpus- and language-agnostic, the
> MedCare numbers are one worked example. Each consumer section below is the
> portability map, graded [G] where the predicates already exist, [H] where
> the arm is analogous-but-unbuilt.
> **Cross-ref:** ruff `ruff_spo_triplet` (the closed vocab + `rekey_exam` /
> `nav_digest` oracles); OGAR `docs/OGAR-CONSUMER-BEST-PRACTICES.md`
> (classid-is-address) + `docs/OGAR-AS-IR.md`; MedCare-rs
> `.claude/knowledge/medcare-transcode-doctrine.md` (the private worked
> ledger this doc generalizes).

______________________________________________________________________

## 0. The one-sentence lesson

**Do not hand-rewrite a legacy app into Rust. Harvest its ugly shape as
SPO facts, let ruff + OGAR refine the mechanical bulk into typed DTOs, and
turn every leftover into the next config fact — the compiler gets the
metal, the proposer gets the slag.**

Everything below is the machinery that makes that one sentence operational,
and safe, on a real codebase.

______________________________________________________________________

## 1. The furnace loop

```
mess → config DTO → proposer placement → residual DTO → next pass
```

Five moves, repeated until the slag pile stops shrinking:

1. **Harvest the ugly shape.** Run the `ruff_*_spo` harvester over the
   legacy source. Out come `(subject, predicate, object)` triples — the
   **ore**. No interpretation yet, just facts.
2. **Re-encode as data-as-config.** A convention table (verbs, scopes,
   aliases, codebook rows, surface kinds, region tokens) — a *runtime*
   file, never Rust literals — tells the refiner how to read the ore.
3. **Let ruff + OGAR lift it into DTOs.** The mechanical bulk (85%) melts
   into typed shapes: a class becomes a `ClassView`, an association becomes
   an `EdgeBlock`, a method becomes an `ActionDef` recipe.
4. **Let the proposer tie loose ends.** What didn't melt cleanly is the
   **slag**: unbound residues, fuzzy bodies, un-mapped screens.
5. **Turn every residual into the next config fact.** Each slag entry names
   *exactly* the config row (an alias, a codebook mint, a recipe) that would
   have melted it — so the next pass melts more. The furnace teaches itself.

**The metal/slag split is a labor split too** (see §9): the melt is
Sonnet grindwork (edit-only, deterministic); the slag triage is Opus
judgment. Never invert it.

### ore/slag decomposes God-objects into SoC

A legacy God-object (a 4,000-line `Form`, a fat `ActiveRecord` model, a
Flask blueprint that does everything) does **not** transcode as one Rust
thing. The ore/slag pass *decomposes* it: each SPO fact is a single
responsibility, and the concepts they cluster into (§3) are the separated
concerns. You never decide the decomposition by hand — it falls out of
which facts share a concept witness.

______________________________________________________________________

## 2. Two oracles — diverse redundancy is the safety

A transcode is only trustworthy when *two independent witnesses* agree the
Rust reproduces the legacy app. Use two that fail differently:

| Oracle | Witnesses | Question it answers | Source of truth |
|---|---|---|---|
| **Value parity** | the legacy **database** | "does the Rust write the same *bytes*?" | MySQL / PostgreSQL rows |
| **Structure parity** | the **Klickwege** (nav/route/view graph) | "does the Rust have the same *shape* — same screens, same order, same regions, same reachability?" | the harvested navigation graph |

They are *diverse* — a value bug (wrong column) and a structure bug (wrong
menu order, missing screen) are caught by different oracles, so a single
blind spot can't hide a regression. The legacy DB is the permanent value
witness (it is never retired); the Klickwege digest is the permanent
structure witness. Any consumer that ships only value parity has a
half-tested UI; any that ships only structure parity has a pretty shell
over wrong data.

**Klickwege = "click-paths".** It is the directed graph of screens, the
navigation edges between them, the views each screen selects, the concepts
each screen surfaces, and — on the render side — the region each control
docks in and its tab order. It is harvested, never authored.

______________________________________________________________________

## 3. The three-axis mint gate — what earns a concept

The furnace's central judgment: **when is a token a real domain concept
(worth a codebook classid) versus just a lookup/enum surface?** A token is
minted **only if witnessed on all three axes**:

```
CONCEPT  ⟺  METHOD ∧ STORAGE ∧ STRUCTURE
```

- **METHOD** — a DAL / service / model family operates on it (there are
  functions whose subject is this thing).
- **STORAGE** — a table / column / schema mirror persists it (the value
  oracle has a home for it).
- **STRUCTURE** — a Klickwege screen gives it a navigational home (the
  structure oracle can reach it).

**Two axes out of three ⟹ it is NOT a concept** — it is a lookup table, an
enum, an RBAC filter, or framework plumbing. Mint it and you pollute the
codebook with non-concepts; every future session then has to reason around
a fake node.

> Worked calls from the MedCare run: `external_practice` had all three
> (own DAL family + `sql_mirror` TTL + its own nav room) → **minted [G]**.
> `user_right` had method + storage but **no Klickwege home** → refused: it
> is the RBAC *global mask*, not a concept (and it later became exactly the
> `global_mask` term in the render equation, §6). That refusal is a *clean
> architectural closure*, not a gap.

The gate is what keeps the codebook honest across consumers: the same three
axes apply whether the METHOD arm is a C# DAL, an Odoo `@api.model`, a Rails
`ActiveRecord` class, or a Flask view function.

______________________________________________________________________

## 4. The no-hand-roll rule + the four sanctioned outputs

**You never hand-author the transcode.** When you feel the urge to write a
Rust table/menu/mapping by hand, that urge is a signal: *extend ruff (or the
config) so the shape is derived instead.* The mantra: **"avoid handroll —
extend ruff so it's data-as-config through better ruff expansion."**

The only things a furnace session is allowed to author are these four, and
each is a *generator or a check*, never the answer itself:

1. **Harvesters** — a new arm on a `ruff_*_spo` walker that emits a new fact
   family (worked example in §5).
2. **Convention configs** — runtime `.conf` rows (verb/scope/alias/codebook/
   surface/region) that tell the refiner how to read the ore.
3. **Proposer recipes** — `(verb, criteria)` codebook entries that correlate
   fuzzy bodies to declarative recipes (see `fuzzy-recipe-codebook.md`).
4. **Residual DTOs + re-derivation tests** — the slag types, and the exam
   that proves a former hand-table was only an unautomated config read.

Three *sanctioned exceptions* — the small, deliberately-inert hand-authored
layers a consumer legitimately owns (from the MedCare render side):

- **ONE generic frame template** (the six-region `base.html`, §6) — one
  layout skeleton, not per-screen markup.
- **The concept→route join table** — the consumer's own claim about which
  route serves each concept (the mirror of the harvest-side room-aliases).
  It is data-as-config the *consumer* owns, not a hand-drawn menu.
- **Inert theme/skin** — CSS that only touches colour, zero structural
  coupling.

If a hand-authored thing is none of these four outputs and none of these
three exceptions, stop: there is a ruff arm or a config row missing.

______________________________________________________________________

## 5. ruff expansion — the mechanical recipe (worked example)

Adding a fact family is always the same three edits. Worked example: the
**region-grammar** family (WinForms Designer layout → six-region frame),
added for MedCare and directly reusable by every consumer's UI layer.

**Edit 1 — the closed vocab** (`ruff_spo_triplet/src/triple.rs`):
add the `Predicate` variants, wire `as_str`/`from_str`/`ALL`/
`default_provenance`, and bump the count-lock test. The vocab is *closed*
(a round-trip test enumerates every predicate) so a new fact can never
silently escape validation. Region-grammar added
`Predicate::{DockedAt, TabOrder, OpensPopup}` (wire `docked_at` /
`tab_order` / `opens_popup`), taking `ALL` from 73 → 76.

**Edit 2 — the harvester arm** (`ruff_<lang>_spo/.../Program.cs` or the
Rust walker): emit the new triples where the AST pattern matches. Region-
grammar added a switch arm: `Dock = DockStyle.X` → `docked_at`, `TabIndex`
→ `tab_order`, `ContextMenuStrip = m` → `opens_popup`. Same `Triple` shape
and provenance as the existing arms; a neutral fixture exercises it.

**Edit 3 — the digest/exam consumer** (`nav_digest` / `rekey_exam`
examples): read the new facts into a diffable golden section. Region-grammar
added the `region=<dock>:<name>` config directive and the `[regions]` /
`[menu-tree]` digest sections (controls grouped by region, ordered by
`tab_order`). Region names are free strings *from config*, never hardcoded.

That is the entire pattern: **fact in the closed vocab → arm that emits it →
golden section that diffs it.** Every consumer's new fact family (Odoo XML
arch, Rails routes, Flask blueprints) is these same three edits.

______________________________________________________________________

## 6. The render side — region grammar + the render equation

The structure oracle has a *render* half: reproducing the legacy UI shape
without emulating its widget tree. The insight: **every legacy screen is a
declarative layout in disguise.** A WinForms `Dock=Fill`, an Odoo
`<form>`/`<tree>`, a Rails ERB region, a Flask/Jinja block — all map onto
**one universal six-region frame**:

```
{ top_bar, left_nav, center, right_panel, bottom_bar, popup }
```

The convention is `predicate → (region, order, interaction)`:
- `docked_at`/arch-tag → **region**,
- `tab_order`/DOM order → **order within region**,
- `opens_popup`/context-menu → **popup interaction**.

**Harvest + reimagine, never emulate.** You do not port the widget tree;
you harvest where things dock and re-render them into the clean frame.

### The render equation (the WideFieldMask cast)

For each region `R`, what actually renders is a masked, ordered projection:

```
live(R)   = region_basis[R] ∩ global_mask ∩ local_mask
render(R) = live(R).ordered_by(harvested_order).as(interaction[predicate])
```

- `region_basis[R]` — every entry the *manifest* declares for region `R`.
  The manifest is the authoritative config (§6b) — there is **no
  reachability drop** in this projection; a manually-added entry is
  rendered, not swallowed.
- `global_mask` — a *session-wide* WideFieldMask = **`RBAC(role)` only**.
  **RBAC / `user_right` IS the global mask** — this is where the concept the
  mint gate *refused* (§3) does its real job: an entry survives only if the
  role may see its concept. (Route *mounting* is NOT a runtime mask — it is a
  test-time drift guard, §6b move 3. Whether an entry renders as a live link
  or a planned placeholder is its own `enabled` flag, not a mask.)
- `local_mask` — a *per-screen* WideFieldMask the active view may narrow
  further (default: show all).

The frame is a **projection under two masks**, not a new struct wrapping the
axes. In the MedCare render, `nav::LayoutFrame` is exactly this projection,
and a **render→parse→re-derive parity test** renders the real page,
re-parses the region's order out of the HTML, and asserts it re-derives the
manifest — the render-side twin of the value oracle.

### Concept is the join key between producer and consumer

The harvested C#/Python/Ruby screen and the Rust route are bridged by the
**concept** (the thing the three-axis gate minted). The screen
`surfaces_concept X`; the consumer's join table maps `X → /route`; RBAC
gates `X`. So the nav order is *where each concept first appears in the
harvested navigation graph* — never hand-picked. The FAITHFUL default is
the legacy first-seen order (preserve muscle memory); an OPTIMISTIC
co-fire reorder ("fires-together-wires-together") is offered as an
**engineer's-gate** candidate, never auto-applied.

______________________________________________________________________

## 6b. The render manifest is EDITABLE CONFIG — manual override

The single rule that keeps a transcode alive instead of frozen: **the
render manifest (the emitted `nav-manifest.json` / config) is the
authoritative, hand-editable source of truth — not a locked build
artifact the deriver owns.** The deriver *seeds* it from the harvest; a
human then overrides it "from the end" (the desired end state). Get this
wrong and the substrate is glued to exactly what the harvest emitted and
impossible to keep working on.

**The anti-pattern that glues it (do NOT ship this):** a hardcoded
reachable-routes set / allow-list in the *consumer code* that silently
**drops** any manifest entry it doesn't recognise. It means adding one
menu item requires editing code AND re-harvesting — the config is no
longer the config. If you find one, delete it (worked instance:
MedCare-rs `nav::reachable_routes`).

**The four moves that keep it editable:**

1. **Manifest is authoritative.** The runtime renders exactly what the
   manifest says, filtered ONLY by *access* masks (RBAC / local /
   patient-context), never by a reachability drop. A manual override is
   rendered, not swallowed.
2. **`enabled` flag = the override switch** (data-as-config; default
   true). A planned end-state entry (`enabled: false`) STILL renders — as
   a disabled placeholder, not a link — so the manifest can carry the
   *desired* end-state menu **before** its route exists. You build toward
   it and flip the flag. This is "thinking from the end": put the whole
   intended menu in config, mark what's live.
3. **Reachability is a TEST-TIME drift guard, not a runtime gate.** A
   *live* entry pointing at an unmounted route fails CI (caught at test
   time); planned entries are exempt. Safety without glue.
4. **Two override surfaces, both data-as-config, neither needs code or a
   re-harvest:** edit the manifest JSON directly, or edit the deriver
   (its `CONCEPT_ROUTE` for live rows + a `PLANNED_OVERRIDES` list for
   end-state placeholders) and re-run it. Document the manifest header as
   the hand-editable source so the next session doesn't treat it as
   regenerated-only.

**Why this is legitimate, not a hack:** the transcode target is an
**object-oriented representation** — ClassView / manifest / config is an
abstraction, so you modify it at the abstraction layer. Data-as-config
means the config author is responsible for the config being correct
(a live entry with a bad route is *their* CI failure, per move 3); the
system's job is to render the config faithfully, not to second-guess it
by dropping rows.

**The consumer render seam that makes it work** (one `NavEntry` shape,
one projection): each entry carries `{concept, scope (global|patient),
route, route_template, order, enabled}`; the frame is a *projection* of
the manifest under the access masks, computing a per-render `href`
(global → bare route; patient → template with the id filled) so the
render never emits an unmounted URL, and reading `enabled` to choose
link-vs-placeholder. Same seam for every consumer; only the manifest
(config) differs.

______________________________________________________________________

## 7. The furnace exam — the re-derivation test

The proof that a transcode is honest: **re-derive the domain concepts from
the harvest, through the pipeline, and check them against the codebook.**
`rekey_exam` does exactly this — `ndjson → reassemble → concept_split
(convention config) → codebook check` — and:

- **green** means a table you might have hand-authored was only an
  unautomated config read (the furnace could regenerate it);
- every **unbound residue** is printed as a ranked candidate — literally the
  next config fact to add.

Corpus-agnostic by construction: the corpus, the convention table, the
codebook rows, and the expected-concept list all arrive as runtime config —
no corpus tokens in the test, no corpus data committed. This is the check
that makes "data-as-config" falsifiable rather than aspirational.

______________________________________________________________________

## 8. Portability map — same furnace, four legacy stacks

The loop is identical; only the *ore source*, the two *oracles*, and which
harvester arm you run differ. For each consumer: which `ruff_*_spo` arm
harvests the ore, what witnesses value vs structure, and which **existing**
closed-vocab predicates already apply (so you extend rather than invent).

### 8.1 odoo → odoo-rs  [G — closest fit to existing vocab]

> **Status update (2026-07-16): the region-grammar row below is BUILT and
> live, no longer prospective.** ruff #79 shipped the Odoo arm
> (`ruff_python_spo::extract_odoo_view_regions` — arch element stack,
> innermost-container docking, depth-0 comodel exclusion, `root` fallback
> for `<xpath>` extension views pending the `inherit_id` join); its merge
> promoted `RegionFact`/`region_triples` into `ruff_spo_triplet` as the
> shared frontend-agnostic carrier (canonical `{screen}.{control}` subject,
> one `build_nav_digest` for Odoo/Rails/WinForms). The consumer half closed
> in odoo-rs #35 (`odoo_regions.conf` + `[regions]` digest + a live-harvest
> byte-parity fuse over real `account` views — no `unmapped:` leak). The
> kausal arms also closed end-to-end (ruff #49 → OGAR #168/#169/#192:
> 11/11 real-source kausal pin), and OGAR #192 shipped the V3 SoA sink
> (`CompiledClass` → 512-B CANON `NodeRow`). Remaining odoo slag: the
> `inherit_id` region join; the od-server hydration writer (odoo-rs #36's
> named follow-up).

- **Ore source:** Python models + XML views. Harvester: **`ruff_python_spo`**.
- **METHOD axis:** `_name`/`_inherit` classes, `@api.model`/`@api.depends`
  methods, `_compute_*` bodies (fuzzy → recipe codebook, `fuzzy-recipe-codebook.md`).
- **STORAGE / value oracle:** **PostgreSQL** (Odoo's own DB) — the value
  witness. Column names follow Odoo's `field → column` convention.
- **STRUCTURE / structure oracle:** the **XML view arch**
  (`<form>`/`<tree>`/`<kanban>`/`<search>`) is the region source; the
  `<menuitem>` + `ir.actions` tree is the Klickwege.
- **Predicates that already apply:** the **Odoo-relational** trio
  (`Target` / `InverseName` / `RelationKind`) was minted *for Odoo*
  Many2one/One2many/Many2many (comodel = target, inverse field, arity). Plus
  the AR-shape and body-mutation families. Region grammar: XML arch tags
  (`form`/`tree`/`kanban`) map to regions exactly like `DockStyle`.
- **classid:** pull via `canonical_concept_id`; `classid = (concept<<16) |
  odoo_app_prefix` (canon-high). Never construct a bridge or copy the codebook.

### 8.2 redmine + OpenProject → openproject-nexgen-rs  [G — AR-shape was built here]

- **Ore source:** Ruby on Rails. Harvester: **`ruff_ruby_spo`** (see the
  existing `ruff_openproject` crate + `body_triage_probe`).
- **METHOD axis:** `ActiveRecord` models — the **AR-shape 32** predicates
  (`declares_association` / `validates_constraint` / `has_callback` /
  `has_scope` / `acts_as` / `includes_module` / …) were cooked on the
  OpenProject + Redmine corpus. `_compute`-equivalent callbacks lower via
  the recipe codebook.
- **STORAGE / value oracle:** the Rails **PostgreSQL/MySQL** schema (via
  `schema.rb` / migrations) — the value witness.
- **STRUCTURE / structure oracle:** **`routes.rb`** (the `RoutesTo` /
  `RouteScope` predicates exist for exactly this) + ERB view templates +
  the app's menu DSL (OpenProject `Redmine::MenuManager` / Redmine menu) as
  the Klickwege.
- **Two apps, one concept space:** Redmine and OpenProject share ancestry;
  harvest both, and the three-axis gate will collapse duplicate concepts
  (same METHOD family + same STORAGE + same STRUCTURE) into one codebook
  node — diverse-redundancy across the *two forks* is a bonus witness.

### 8.3 WoA → woa-rs  [H — Flask arm analogous to Odoo's Python arm]

- **Ore source:** Python / Flask / SQLAlchemy. Harvester:
  **`ruff_python_spo`** (same walker as Odoo; different framework idioms).
- **METHOD axis:** SQLAlchemy models + Flask view functions; the
  body-mutation family (`WritesField`/`Calls`/…) captures the handler logic.
- **STORAGE / value oracle:** **MySQL** (Stefan's DB) — the value witness.
  woa-rs additionally carries an **OGIT-TTL** as the *target* spec, so TTL
  vs Python disagreement is a third witness (TTL wins → RFC).
- **STRUCTURE / structure oracle:** Flask **`@bp.route` blueprints** (map to
  `RoutesTo`/`RouteScope`) + **Jinja** templates (region source) + the nav
  in the base template as the Klickwege.
- **Note:** woa-rs already runs a behaviour-parity harness against the
  Python reference (`tests/parity/`) — that is the value oracle; add the
  Klickwege structure oracle (route + template harvest) as the diverse
  second witness.

### 8.4 The reference: MedCare → MedCare-rs  [G — the fully-run example]

- **Ore source:** C# WinForms/DevExpress. Harvester: **`ruff_csharp_spo`**.
- **Value oracle:** MySQL. **Structure oracle:** the golden Klickwege digest
  (`[klickwege]`/`[views]`/`[concepts]`/`[regions]`/`[menu-tree]`).
- **Region grammar:** `DockStyle` → region, `TabIndex` → order,
  `ContextMenuStrip` → popup. Render: `nav::LayoutFrame` (the render
  equation), themes as inert skin, render→parse→re-derive parity test.
- This is the worked ledger; when in doubt, read what MedCare-rs actually
  did (its private `medcare-transcode-doctrine.md`) and mirror the *method*,
  not the German tokens.

______________________________________________________________________

## 9. Model policy + the 5+3 council

**The grindwork/accumulation split maps onto the metal/slag split:**

- **Metal (grindwork) → Sonnet, edit-only.** Writing a file from a spec,
  a harvester arm, running a fixture, drafting a golden section. Bounded
  input, known output shape. Sonnet workers **do not** run full `cargo`
  builds and **do not** spawn worktrees — the orchestrator compiles
  **once**, centrally, in the shared `target/` (one build, not twelve).
- **Slag (accumulation) → Opus.** Triaging residues, judging a mint against
  the three axes, tracing a concept across method+storage+structure,
  synthesizing across sources. Judgment that only makes sense with several
  inputs held together.

**Concrete test before spawning:** *"does this agent read N sources and
produce something that only makes sense with all N in mind?"* Yes → Opus.
One-source-in-one-shape-out → Sonnet. Never `haiku`.

**The 5+3 council for delicate mints.** When a mint or a family split is
ambiguous, do not free-style: write a spec so detailed the council can't
divert, cast **5 research savants** (parallel, Sonnet) → **consolidate
first** into a draft → cast **3 brutal reviewers** (Opus) on the draft only
→ fix → land with board hygiene. The strict order (consolidate *before*
review) is the anti-mush protocol. This is how MedCare's admin-plane and
health mints were gated.

______________________________________________________________________

## 10. Anti-patterns — the furnace catches these

- **Hand-drawn menu / table / mapping.** If it's not one of the four
  sanctioned outputs or three exceptions (§4), a ruff arm or config row is
  missing. Extend the producer, don't author the answer.
- **Minting a two-axis token.** Method+storage but no Klickwege home = it's
  a lookup/enum/RBAC filter, not a concept. Refusing it is a closure, not a
  gap (`user_right` → global_mask).
- **A hardcoded reachability gate in consumer code** (a route allow-list
  that silently drops manifest entries). This is the glue that freezes
  the transcode — adding a menu item then needs a code edit + re-harvest.
  The manifest is the config; render what it says, mark planned entries
  `enabled: false`, and move reachability to a test-time drift guard (§6b).
- **Emulating the widget tree.** Porting WinForms/XML/ERB layout verbatim
  instead of harvesting-and-reimagining into the six-region frame.
- **One oracle only.** Value parity without structure parity ships a
  half-tested UI; structure without value ships a pretty shell over wrong
  data. Both, always.
- **Constructing a `*Bridge` / copying the codebook in the consumer.** Pull
  classids via `*Port::class_id` / `canonical_concept_id`; never
  re-implement the Core locally (OGAR `ogar-consumer-preflight`).
- **Corpus tokens in a public repo.** The harvest ndjson and any digest that
  carries screen/control/field *names* stay in the private consumer repo.
  ruff / OGAR / lance-graph get only neutral fixtures and the *method*.
- **Sonnet workers running `cargo build` in their own worktree.** 12× the
  `target/` residue and 12 cold compiles. Edit-only; orchestrator builds once.

______________________________________________________________________

## 11. The shortest possible checklist

1. Run the `ruff_<lang>_spo` harvester → ore ndjson (private).
2. Write the convention config (verbs/scopes/aliases/codebook/surface/region).
3. `rekey_exam` → read the ranked slag → add the next config fact → repeat.
4. Mint a concept **only** on METHOD ∧ STORAGE ∧ STRUCTURE (5+3 if delicate).
5. Build the Klickwege golden digest → structure oracle; wire the value
   oracle against the legacy DB.
6. Render side: derive the six-region frame from the digest (concept→route
   join), cast under global∩local masks, prove with render→parse→re-derive.
7. Every new fact family = three edits (vocab → arm → digest section).
8. Metal→Sonnet edit-only; slag→Opus; orchestrator compiles centrally.

**The compiler gets the metal. The proposer gets the slag. The furnace
teaches itself.**
