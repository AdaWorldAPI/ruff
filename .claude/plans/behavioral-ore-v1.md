# Ordered behavioral ore for C++ — plan v1

**Status:** IN PROGRESS. The extractor is the deliverable here; the tokenizer
that consumes it is a separate, downstream experiment and nothing in this plan
asserts a result for it.

## 1. The baseline defect (this is a FINDING, not plumbing)

> **The C++ harvest representation cannot carry behavior, because it is a set.**

Two independent causes, both in `ruff_cpp_spo`:

1. **`method_body_arm` ends by collapsing the traversal into sets.**
   `clang_walker.rs:1011-1025` runs `facts.sort(); facts.dedup();` over all
   five vectors, unconditionally, in the one function that produces a
   `BodyArm`. Adjacency, repetition and interleaving are gone before any
   consumer sees a fact. `expand.rs:904-943` then emits by fixed category
   (reads → raises → writes → guarded → calls), alphabetical within category,
   so nothing downstream can recover source order either.

2. **`walk_body` gives the structured constructs no match arm.**
   Only `ThrowExpr`, `BinaryOperator`, `CompoundAssignOperator`,
   `UnaryOperator`, `CallExpr`, `IfStmt` and `MemberRefExpr` are matched
   (`clang_walker.rs:1035-1110`). `ForStmt`, `WhileStmt`, `DoStmt`,
   range-for, `SwitchStmt`, `CXXTryStmt`, `CXXCatchStmt`, `LambdaExpr`,
   `ReturnStmt` fall through `_ => walk_body(...)`: their CONTENTS are
   visited, their BOUNDARIES are not recorded anywhere. A write inside a loop
   and a write in straight-line code are the same fact. `IfStmt` is matched
   only to thread the J1 guard; it emits no event of its own.

**Consequence.** The recurring shapes a behavioral vocabulary would be made of
— an iterator loop, a lock/mutate/unlock triple, an RAII scope, a
dispatch-then-propagate chain, a try/catch compensation — are precisely the
ones that require order and scope, and every one of them is unrepresentable
today. A sequence learner run against this output is learning words from a bag
of letters.

**This is not an argument against the five sets.** They are the right input to
`recipe::classify`, which is a set-membership classifier by design and works.
The defect is that the harvest DISCARDS the sequence rather than emitting both.

### Measured on the control fixture
See `§4`. The measurement is reported by `ore.py`'s loss table and is repeated
in the commit that lands the extractor; it is not asserted here in advance.

## 2. What this adds — three things, preserved SEPARATELY

| preserved | artefact | deduped? |
|---|---|---|
| event identity | `symbols.tsv` | yes — the only place |
| event order | `events.tsv` | **never** |
| structural context | `scopes.tsv` | n/a |

The same fact occurring twice is two rows in `events.tsv` pointing at one row
in `symbols.tsv`. Keeping the canonical symbol table and the occurrence stream
apart is what lets a consumer choose its own granularity without the extractor
having chosen for it.

## 3. Neutrality rules — the ore must not pre-solve the experiment

1. **No inferred motifs in the alphabet.** The 20 kinds are primitive
   (`ScopeEnter`/`Read`/`Call`/`Condition`/…). There is no `RAII`,
   `IteratorLoop`, `VirtualDispatch` or `Move` kind. A `lock_guard` local is
   `Decl` + a type symbol; `std::move(x)` is `Call` + a callee symbol. Those
   concepts are candidate LEARNED tokens; emitting them would be the
   tokenizer's answer written into its input.
2. **No consumer vocabulary.** Nothing about classids, facets, rails, masks or
   any 96-bit layout appears in the ore or in this crate. The ore does not
   know those exist.
3. **Identities are verbatim, never hashed.** Different consumers hash
   differently; a hash in the ore forces one of them.
4. **Lexical order and control flow stay distinguishable.** Every event carries
   its source byte `anchor`. `control` carries only structurally-certain
   relations. **libclang's C API exposes no CFG** — successor/predecessor
   edges are not available and are never fabricated. A later probe may ask
   whether CFG adjacency beats lexical adjacency; it cannot ask it from these
   files, and must say so rather than assume the two are the same.

## 4. The control

`for (…) { if (…) foo(); else bar(); }` — the expected event sequence was
hand-written BEFORE the extractor existed. Where libclang disagrees (AST child
order for `ForStmt` is init/cond/inc/body, which is not source order), the
disagreement is reported and the `anchor` column keeps both orders separable.
It is not resolved by editing the expectation to match the code.

## 5. Fences

- **Additive only.** `method_body_arm`, `BodyArm`, `CppMethod`, `expand` and
  the emitted ndjson are byte-identical after this change. A test asserts that
  collapsing the new events to the five sets reproduces `method_body_arm`
  exactly — proving the change is about what is PRESERVED, not about what a
  fact is.
- **No new predicate.** The closed vocabulary is untouched; the ore is TSV
  beside the manifest, not a triple.
- The extractor is a second walk over the same cursor, in its own module.
