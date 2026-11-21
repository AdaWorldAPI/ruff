# From a Rails harvest to the register contract: what a session must carry forward

> **Type:** knowledge (methodology plus the receipts behind it).
>
> **READ BY:**
>
> - any session that extends `ruff_ruby_spo` (class rows, macros, functions,
>   routes, schema);
> - any session that consumes its facts in zammad-rs `rails-codegraph` (the
>   crosswalk, the intake arm, Klickwege);
> - any session that maps harvested tables onto lance-graph lanes and checks
>   them against PostgreSQL;
> - the `fuzzy-proposer` agent, when a body fact depends on which constant a
>   name resolves to.
>
> **Status:** FINDING where a receipt is named. Everything else is a working
> rule and is marked as one.
>
> **Written:** 2026-11-21, after ruff #146–#149, zammad-rs #93–#97 and
> lance-graph #2152.
>
> **Cross-ref:**
>
> - `consumer-transcode-furnace-playbook.md`: the loop this doc sits in.
> - `fuzzy-recipe-codebook.md`: the body-fact codebook.
> - lance-graph `.claude/knowledge/register-contract-reconciliation.md` and
>   `.claude/knowledge/morton-vs-ordinal-rows.md`: the read side.

## 1. Resolve a constant the way Ruby does, or every downstream fact inherits the error

A constant in Ruby is resolved through `Module.nesting` at the place it was
written, not through the name the class ends up with.

- In `module Queries; class UpdateContract < BaseContract`, the superclass is
  read in `[Queries]` and names `Queries::BaseContract`.
- In `class Queries::UpdateContract < BaseContract`, the same text is read at
  the top level and names `::BaseContract`.
- `::X` searches the top level only.
- A constant named in a class body (`include X`) is searched in three places,
  in order: the lexical scopes (the class itself first), then each
  superclass's scope, then the top level.

What ruff does: since #149, `ClassMacroRow` and `ClassFunctions` carry
`nesting`, innermost first, and the class walk hands a `ClassScope` (class,
superclass, nesting) to its callback.

What the consumer does: zammad-rs `rails-codegraph`'s `ConstRef { path,
nesting, absolute }` follows those rules (`resolve`, `resolve_include` in
`src/crosswalk.rs`).

**Receipt:** zammad-rs#93. Over OpenProject, the source-graded loose
predicates of the crosswalk fell from 11 to 2 once resolution followed
nesting instead of a suffix match.

**Rule:** a harvested row that names a constant carries the scope it was
written in. A consumer never resolves by suffix or by "the only class with
that last segment". That guess is right often enough to pass a fixture and
wrong exactly where two modules reuse a name.

## 2. Configuration is ore too: detect it from the source, then diff it against the hand-written config

Hand-written config lists (roots, macro names, receivers, gems) are claims
nobody measured. Most of them are stated in the source the harvest already
reads:

- `config.load_defaults` in `config/application.rb`;
- `self.abstract_class = true` on `ApplicationRecord`;
- `attribute_method_suffix` calls in activemodel;
- Ruby core method names in rbs `core/*.rbs`.

zammad-rs #94 built an **intake arm**. Each detector emits a fact `(app,
config key, value)` with support, total, an integer NARS truth and up to
three witnesses. Then a diff gives every hand key exactly one verdict:
`agrees`, `detected_superset`, `detected_subset`, `differs`,
`not_detected` (with a reason: `policy`, `environment`, `no_detector`) or
`detected_only`. The run fails if the verdicts do not add up to the hand
keys.

**Receipts:**

- Zammad (`harvest/intake-zammad.json`): 32 facts, 23 detected. Over the hand
  config: 3 agree, 1 differs (`framework_roots`), 1 is a detected subset
  (`filter_macros`), 3 are detected only (`definer_macros`, `model_roots`,
  `include_macros`), 20 are not detected. Of those 20, one is `environment`
  (`source_env`); the rest have no detector yet.
- OpenProject, `from_intake = true` with the detected keys removed from the
  table: K10 measured 1385 / 1018 / 367, against 1364 / 1000 / 364 from the
  hand config. The arm finds slightly more than the hand list, not less.

**Rules:**

- A detector whose source is absent says so and detects nothing. An empty
  list is never reported as a finding.
- No app or gem names appear in detector code. A name in the code is a
  config value that escaped the intake.
- Present hand values still win, so a config shrinks to what the arm cannot
  know.

## 3. Count what the harvest does not understand

Since #146, the routes arm counts every unknown DSL call (`use_doorkeeper`,
`totally_unknown_dsl :foo`) under its own name instead of dropping it.

**Rule:** a harvester that silently skips a construct turns "we did not
read it" into "it is not there". The count is the gap in the harvester,
made visible.

## 4. What the facts become on the read side, and which facts are still missing

The table facts end up as lanes in lance-graph. A row is read in place at
one position, `identity[index].key : identity[index].value`, through one
check per table (lance-graph `lance-graph-pg-row`). PostgreSQL is the oracle.

Three things were measured there that a harvest must feed.

**Absent is one code (lance-graph DECISION EXO-372).** `''`, `$null`, NaN,
NULL, missing and deleted are all −0: presence 0 and value 0. `0` is a
number. Whether PostgreSQL shows an absent text cell as `NULL` or as `''`
depends on the column's declared default.

- Receipt: zammad-rs's wide capture holds 1958 `''` cells, from 16 nullable
  text columns that never also hold NULL.
- The pg-row reader takes this per column (`AbsentAs`). The only source of
  that choice is the schema: `null: false, default: ""`.
- The gap: `ruff_ruby_spo`'s schema stratum (`src/schema.rs`) emits
  `not_null` but not the column default. The next schema extension is to
  emit the default value, so the consumer can set `AbsentAs` from facts
  instead of by hand. Not built yet.

**Deleted rows keep their values.**

- Receipt: a left join over a deleted parent returned 547 stale rows
  (zammad-rs#95) until reads refused dead positions.
- A harvest that models soft deletes, such as `acts_as_paranoid` or a
  `deleted_at` column, is a liveness fact, not a value column.

**Relations are bags, coordinates are bijections.**

- Receipts: zammad-rs#96, and lance-graph #2149 / #2152.
- A has-many read keeps duplicates, as PostgreSQL's join does.
- A harvested foreign key names a relation between classes. It is never
  resolved row by row at read time; the classes are placed on shared
  coordinates at intake (lance-graph `classid-on-do-call-shape.md` §0).

## 5. Session traps these slices paid for

- **A committed artifact can be written by a disabled build.** zammad-rs#96
  merged a census JSON written during a `disable-*` run, so its source
  callbacks were doubled. #97 regenerated it.
  - Rule: regenerate committed harvests and censuses only from the default
    feature set.
  - Rule: a census logs steps only inside an open window. An unwindowed log
    grows without bound and mixes calls.
- **Commit before a disable run.** The ruff `AGENTS.md` trap holds across
  repos. The restore is `git checkout`, which goes back to the last commit.
- **Read the source before trusting a number from it.**
  - Reading `holo_interval_probe.rs` before writing about Morton order showed
    the probe uses Morton rows by construction. A table's rows are not in
    Morton order.
  - Reading `dr.rs` showed the DR Morton tile address has no read-path caller.
- **A green fixture proves the shape it holds.** The nesting fix needed both
  spellings (`module Queries; class X < Base` and `class Queries::X < Base`)
  in one test, asserting they resolve differently.

## 6. Where to look next

- `ruff_ruby_spo` schema: emit `default:` per column (§4), and soft-delete
  markers as a liveness fact.
- The intake arm: the 19 `no_detector` keys in Zammad are its backlog. Each
  one is either a detector to write or a policy value to keep by hand.
- lance-graph D-REGC-13: a DR projection descriptor that names its row order
  (`Ordinal` or `Morton { width }`). A harvested table is `Ordinal`.
