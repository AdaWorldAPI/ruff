# R2IL pass-1 TRIAGE RESULT

Caps in force: `R2IL_HARVEST_MAX_FUNCS=200`, `R2IL_HARVEST_MAX_SECTION_BYTES=262144`

## Pre-registered bars (stated BEFORE the measured section below)

- **B1 — conservation (absolute).** `dropped == 0` and `harvested == classified + residual`. Any violation **KILLS** the pass: the enumerator, not the corpus, is wrong. Not a percentage.
- **B2 — coverage of the declared seven.** Of ore facts whose parent opcode is one of `{Copy, IntAdd, Load, Store, CBranch, Call, Return}`, **>=99% classify -> PASS; <90% -> KILL.** The 90-99% band is INVESTIGATE (expected causes: operand rows with no convention row at their address, `CallSite` rows with no `direct_target` — both legitimate slag under a parent that classified).
- **B3 — the slag is named and addressed, not lumped.** `residual > 0`, distinct `shape_id` count **>= 5**, `dominant_share() < 0.60`, and **every** residual except `NoFacetCoordinate` carries `at.is_some()`. `residual == 0` is a **KILL** too — it means someone widened the ladder.

Also **pre-register a prediction that is NOT a bar** (so it can be wrong without moving a goalpost): on an x86-64 corpus `Copy/IntAdd/Load/Store` dominate, so pass 1 is expected to classify roughly **60-80%** of all `Op` facts. Record the measured figure either way.

---

## Measured

Functions harvested: 143

Conservation line: harvested 54304 / classified 17557 / residual 36747 / dropped 0

**B1: PASS** — dropped == 0: true; harvested == classified + residual: true

**B2: INVESTIGATE** — 17528 classified / 19198 total ore facts under a seven-opcode parent = 91.30%.

  Derivation note: `ResidualFact` does not carry its parent opcode directly, so the denominator's residual half is APPROXIMATED by summing residual reasons that can *only* fire on a row whose parent op is one of the seven (`no_convention_row_at_address`, `indirect_target`, `memory_object_escaped`, `op_site_join_mismatch`, `custom_space_not_in_convention`, `facet_overflow_at_key`) — reasons that can only fire on a non-seven or no-parent-op row (`opcode_not_in_convention`, `user_op_not_in_convention`, `phi_fan_in_exceeds_predecessors`, `variadic_arity`, `no_facet_coordinate`) are excluded. Labelled APPROXIMATION, not exact — see the module doc comment above `SEVEN_ELIGIBLE_RESIDUAL_REASONS`.

**B3: PASS** — residual > 0: true (36747); distinct shape_id count: 43 (>=5: true); dominant_share: 0.215 (<0.60: true).

  Spot check: every grouped bucket except no_facet_coordinate reports an example facet address.

**Non-bar prediction, measured:** 5340 / 37728 `Op` facts classified = 14.15% (predicted 60-80%; OUTSIDE the predicted band — recorded honestly, not a bar).
