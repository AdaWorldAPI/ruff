# `.claude/knowledge/` — curated methods for the SPO/transcode side of ruff

Each doc teaches a **method** (the fishing), not a one-off answer, and carries a
`READ BY:` header naming which sessions/agents must load it before producing
output in that domain. Grep here before re-deriving.

| doc                        | teaches                                                                                                                                                                                           | READ BY                                                                                                                         |
| -------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `fuzzy-recipe-codebook.md` | cook a `(verb, criteria)` recipe codebook from imperative method-body facts; correlate fuzzy bodies to declarative recipes; roll the buckets until the residue is irreducible; collect the jitter | any session harvesting body facts (`ruff_{ruby,python,csharp,cpp}_spo`) or lowering the OGAR DO-arm; the `fuzzy-proposer` agent |
| `consumer-transcode-furnace-playbook.md` | the whole legacy-app→Rust loop *around* the codebook: ore/slag furnace, the two parity oracles (value + Klickwege structure), the three-axis mint gate, the no-hand-roll rule, ruff-expansion mechanics, the six-region render equation, and a per-consumer portability map | any session transcoding a legacy app — **odoo→odoo-rs**, **redmine+OpenProject→openproject-nexgen-rs**, **WoA→woa-rs** (worked ref: **MedCare→MedCare-rs**); anyone adding a harvester arm, a vocab predicate, or a consumer render surface |

Also here: `rails-harvest-to-register.md` covers what the Rails harvest must
carry forward to the read side:

- resolving constants through `Module.nesting`;
- detecting config from the source and diffing it against the hand config
  (the intake arm);
- counting unknown DSL calls;
- which schema facts (the column default, soft deletes) the register contract
  needs.

Read it before you extend `ruff_ruby_spo` or consume its facts in zammad-rs
`rails-codegraph`.

Agents live in `../agents/`. The `fuzzy-proposer` card is the operator for
`fuzzy-recipe-codebook.md`.

## The SPO frontends and their body-fact "DTO arm" status (2026-07-06)

The recipe codebook runs on the `(writes, reads, raises, calls)` quartet +
private-helper split on `ruff_spo_triplet::Function`. `ruff_ruby_spo` and
`ruff_csharp_spo` emit the full arm today (writes/reads/raises/calls ✅,
syntax-only — SemanticModel upgrade pending — helpers ✅ via `has_visibility`);
**C++ and Python still need it** (pure AST-walk work per frontend, zero
shared-IR change — the predicates already `expand()`). See
`fuzzy-recipe-codebook.md` §2 for the coverage table and how to add the arm.
