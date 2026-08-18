//! Stage 4 — DTO / codebook factoring.
//!
//! Feeds lance-graph's `ogar_codebook` — **read-only**. This module never constructs a parallel
//! codebook of its own; it produces a LOCAL, deterministic interning table plus a measurement of
//! what that table would save, so PR 2/3 can wire the real mint against
//! `lance_graph_contract::ogar_codebook` (the `NETWORK_LAYER = 0x0804` analog) with numbers in
//! hand instead of a guess.
//!
//! Three concerns, kept separate rather than folded into one bag of strings:
//!
//! - **Names that need interning** ([`VocabTable`], via [`VocabHarvest::ssa_names`] /
//!   [`VocabHarvest::op_spaces`] / [`VocabHarvest::object_spaces`]) — deterministic,
//!   order-independent, built from a `BTreeSet` so the table is a pure function of the name
//!   *set*.
//! - **Numbers that are already dense and need only counting** ([`VocabHarvest::userops`]) —
//!   `CallOther`'s `userop: u32` is already an integer id; wrapping it in a [`VocabTable`] would
//!   spend a second id space on a value that already has one, so it is **counted, not
//!   interned**.
//! - **The typed-vs-string measurement** ([`custom_space_ids_from_blocks`] vs
//!   [`parse_custom_space_id`] / [`custom_space_id_from_var_name`]) — `SpaceId::Custom(u32)` is
//!   fully recoverable from the **typed** R2IL side; the plan forbids parsing display strings as
//!   a data path (§2), so the string-side functions exist solely to put a NUMBER on how much of
//!   that typed truth a naive string-based harvest would have recovered anyway. `None` from
//!   either string function is not a defect — it is the measurement working as intended.

use std::collections::{BTreeMap, BTreeSet};

use r2il::{R2ILBlock, R2ILOp, SpaceId};
use r2ssa::{InstPayload, ObjectKind, SSAOp};

use crate::behavior::FunctionBehavior;

/// A deterministic, dense id into a [`VocabTable`]. 4 bytes — the whole point of interning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VocabId(pub u32);

/// Deterministic, **order-independent** interning of a name set.
///
/// Built from a `BTreeSet`, ids assigned in sorted order — the table is a pure function of the
/// name *set*, never of insertion order or of how many times a name was seen.
///
/// `JUDGMENT`: there is deliberately **no** public incremental `intern(&mut self)` in PR 1. Two
/// build paths — one incremental, one batch — would let the same name end up with a different id
/// depending on which one ran and in what order, which is exactly the nondeterminism this type
/// exists to rule out. A caller who needs to grow a table re-derives it via [`VocabTable::from_names`]
/// over the union of names, not by mutating one in place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VocabTable {
    by_name: BTreeMap<String, VocabId>,
    names: Vec<String>,
}

impl VocabTable {
    /// Build from any iterable of names — owned or borrowed, any order, duplicates welcome. See
    /// the struct docs for why none of that can affect the result.
    pub fn from_names<I, S>(names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let unique: BTreeSet<String> = names.into_iter().map(Into::into).collect();
        let names: Vec<String> = unique.into_iter().collect();
        let by_name = names
            .iter()
            .enumerate()
            .map(|(idx, name)| (name.clone(), VocabId(idx as u32)))
            .collect();
        Self { by_name, names }
    }

    /// The id for a name, if it was interned.
    pub fn id_of(&self, name: &str) -> Option<VocabId> {
        self.by_name.get(name).copied()
    }

    /// The name for an id, if it is in range.
    pub fn name_of(&self, id: VocabId) -> Option<&str> {
        self.names.get(id.0 as usize).map(String::as_str)
    }

    /// Number of distinct interned names.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    /// `true` iff nothing was interned.
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Every `(name, id)` pair, in sorted-name order — the same order ids were assigned in.
    pub fn iter(&self) -> impl Iterator<Item = (&str, VocabId)> + '_ {
        self.by_name.iter().map(|(name, id)| (name.as_str(), *id))
    }

    /// Total bytes of every **unique** interned name — the dictionary's own one-time size, not
    /// the cost of referencing it from N occurrences. See [`VocabStats::ssa_name_bytes`] /
    /// [`VocabStats::interned_id_bytes`] for the occurrence-level comparison.
    pub fn name_bytes(&self) -> usize {
        self.names.iter().map(String::len).sum()
    }
}

/// One function's vocabulary harvest, factored by concern rather than dumped into one bag of
/// strings.
///
/// Feeds lance-graph's `ogar_codebook` — **read-only**; see the module docs. PR 1 produces this
/// local table and the [`VocabStats`] measurement; PR 2/3 wire the real mint.
#[derive(Debug, Clone, Default)]
pub struct VocabHarvest {
    /// `SSAVar::name` over every entry of `SsaGraph::values` — every SSA value in the function
    /// (phi outputs included), deduplicated **by name**, not by `(name, version)`. Two versions
    /// of the same physical register (e.g. two writes to `reg:38`) intern to the same
    /// [`VocabId`].
    pub ssa_names: VocabTable,
    /// `SSAOp`'s own `space: String` field, present on the seven memory ops (`Load`, `Store`,
    /// `LoadLinked`, `StoreConditional`, `AtomicCAS`, `LoadGuarded`, `StoreGuarded`).
    /// Debug-formatted upstream (`"Ram"`, `"Custom(7)"`, …) — see [`parse_custom_space_id`]'s doc
    /// comment for the exact upstream call site this mirrors.
    pub op_spaces: VocabTable,
    /// `ObjectKind::Global { space, .. }`'s `space` string, over every object in the function's
    /// `ObjectModel`.
    pub object_spaces: VocabTable,
    /// `CallOther`'s `userop: u32` — **counted, not interned** (see the module docs). Key =
    /// userop index, value = number of `CallOther` sites mentioning it.
    pub userops: BTreeMap<u32, usize>,
    /// [`parse_custom_space_id`] applied to every observed `op_spaces` string, kept only where it
    /// parsed. The STRING-recovered half of the [`custom_space_ids_from_blocks`] measurement —
    /// compare against that typed oracle, never trust this set alone.
    pub custom_spaces_from_strings: BTreeSet<u32>,

    // Private bookkeeping so `stats()` can report the interning-savings measurement without
    // re-walking the source `FunctionBehavior` a second time. `JUDGMENT`: kept out of the public,
    // concern-factored field list above because these two are pure occurrence counters, not a
    // concern in their own right.
    total_ssa_values: usize,
    ssa_name_bytes_raw: usize,
}

impl VocabHarvest {
    /// Harvest one function's vocabulary. Deterministic: every table is built via
    /// [`VocabTable::from_names`] over a `BTreeSet`, and every upstream container walked here
    /// (`SsaGraph::insts`, `SsaGraph::values`, `ObjectModel::objects`) is itself ordered
    /// (`Vec` / `BTreeMap`) — no `HashMap` iteration leaks into the result.
    pub fn from_behavior(behavior: &FunctionBehavior) -> Self {
        let graph = behavior.values();

        let mut total_ssa_values = 0usize;
        let mut ssa_name_bytes_raw = 0usize;
        let mut ssa_name_list: Vec<String> = Vec::with_capacity(graph.values.len());
        for value in &graph.values {
            total_ssa_values += 1;
            ssa_name_bytes_raw += value.var.name.len();
            ssa_name_list.push(value.var.name.clone());
        }
        let ssa_names = VocabTable::from_names(ssa_name_list);

        let mut op_space_names: BTreeSet<String> = BTreeSet::new();
        let mut userops: BTreeMap<u32, usize> = BTreeMap::new();
        let mut custom_spaces_from_strings: BTreeSet<u32> = BTreeSet::new();

        for inst in &graph.insts {
            let InstPayload::Op(op) = &inst.payload else {
                continue;
            };
            if let Some(space) = ssa_op_memory_space(op) {
                op_space_names.insert(space.to_string());
                if let Some(id) = parse_custom_space_id(space) {
                    custom_spaces_from_strings.insert(id);
                }
            }
            if let SSAOp::CallOther { userop, .. } = op {
                *userops.entry(*userop).or_insert(0) += 1;
            }
        }
        let op_spaces = VocabTable::from_names(op_space_names);

        let mut object_space_names: BTreeSet<String> = BTreeSet::new();
        for fact in behavior.objects().objects.values() {
            if let ObjectKind::Global { space, .. } = &fact.kind {
                object_space_names.insert(space.clone());
            }
        }
        let object_spaces = VocabTable::from_names(object_space_names);

        Self {
            ssa_names,
            op_spaces,
            object_spaces,
            userops,
            custom_spaces_from_strings,
            total_ssa_values,
            ssa_name_bytes_raw,
        }
    }

    /// The measurement: vocabulary size per concern, plus the byte-cost-of-interning comparison
    /// for `ssa_names` (by far the largest of the three tables in practice).
    pub fn stats(&self) -> VocabStats {
        VocabStats {
            unique_ssa_names: self.ssa_names.len(),
            unique_op_spaces: self.op_spaces.len(),
            unique_object_spaces: self.object_spaces.len(),
            unique_userops: self.userops.len(),
            userop_mentions: self.userops.values().sum(),
            unique_custom_spaces_from_strings: self.custom_spaces_from_strings.len(),
            total_values: self.total_ssa_values,
            ssa_name_bytes: self.ssa_name_bytes_raw,
            interned_id_bytes: self.total_ssa_values * std::mem::size_of::<VocabId>(),
        }
    }
}

/// The measurement PR 1 promised: how big each vocabulary concern is, and what interning
/// `ssa_names` would cost versus save.
///
/// `total_values` / `ssa_name_bytes` / `interned_id_bytes` are `JUDGMENT` (the spec names the
/// fields but not their exact formula): `total_values` is the **raw**, pre-name-dedup count of
/// `SsaGraph::values` entries (so a register written twice contributes 2, even though it
/// contributes only 1 to `unique_ssa_names`); `ssa_name_bytes` is what it would cost to store
/// every one of those `total_values` occurrences as its own inline string, uninterned;
/// `interned_id_bytes` is the alternative — one 4-byte [`VocabId`] per occurrence, referencing
/// the deduplicated `ssa_names` table. Add `ssa_names.name_bytes()` (the one-time dictionary
/// cost) to `interned_id_bytes` for the full interned total, and compare against `ssa_name_bytes`
/// alone for the naive total.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VocabStats {
    pub unique_ssa_names: usize,
    pub unique_op_spaces: usize,
    pub unique_object_spaces: usize,
    pub unique_userops: usize,
    pub userop_mentions: usize,
    pub unique_custom_spaces_from_strings: usize,
    pub total_values: usize,
    pub ssa_name_bytes: usize,
    pub interned_id_bytes: usize,
}

/// The address-space field carried directly by the seven memory [`SSAOp`] variants (distinct from
/// the space of any operand — an op's `addr` var is typically register/const-space and holds an
/// address value; this field says which memory space that address refers *into*).
fn ssa_op_memory_space(op: &SSAOp) -> Option<&str> {
    match op {
        SSAOp::Load { space, .. }
        | SSAOp::Store { space, .. }
        | SSAOp::LoadLinked { space, .. }
        | SSAOp::StoreConditional { space, .. }
        | SSAOp::AtomicCAS { space, .. }
        | SSAOp::LoadGuarded { space, .. }
        | SSAOp::StoreGuarded { space, .. } => Some(space.as_str()),
        _ => None,
    }
}

/// The address-space field carried directly by the seven memory [`R2ILOp`] variants — the typed,
/// pre-rename counterpart of [`ssa_op_memory_space`], used by [`custom_space_ids_from_blocks`].
fn r2il_op_memory_space(op: &R2ILOp) -> Option<SpaceId> {
    match op {
        R2ILOp::Load { space, .. }
        | R2ILOp::Store { space, .. }
        | R2ILOp::LoadLinked { space, .. }
        | R2ILOp::StoreConditional { space, .. }
        | R2ILOp::AtomicCAS { space, .. }
        | R2ILOp::LoadGuarded { space, .. }
        | R2ILOp::StoreGuarded { space, .. } => Some(*space),
        _ => None,
    }
}

/// The TYPED oracle for custom space ids: walks `Varnode::space` (every operand `inputs()` and,
/// if present, `output()`) and, for the seven memory ops that also carry an address-space field
/// of their own, that field too — on the **R2IL** side, before any SSA rename touches it. No
/// parsing, no strings. [`VocabStats::unique_custom_spaces_from_strings`] is measured against
/// this, never the other way around.
pub fn custom_space_ids_from_blocks(blocks: &[R2ILBlock]) -> BTreeSet<u32> {
    fn note(space: SpaceId, ids: &mut BTreeSet<u32>) {
        if let SpaceId::Custom(raw) = space {
            ids.insert(raw);
        }
    }

    let mut ids = BTreeSet::new();
    for block in blocks {
        for op in &block.ops {
            if let Some(space) = r2il_op_memory_space(op) {
                note(space, &mut ids);
            }
            for varnode in op.inputs() {
                note(varnode.space, &mut ids);
            }
            if let Some(varnode) = op.output() {
                note(varnode.space, &mut ids);
            }
        }
    }
    ids
}

/// Recover a possible custom-space ordinal from an upstream **space string** — the kind of string
/// that ends up in an [`SSAOp`]'s own `space: String` field.
///
/// Two known upstream shapes, both verified against `r2sleigh` source this session:
/// - `"Custom(7)"` — the function-level rename's `format!("{:?}", space)`
///   (`r2ssa/src/rename.rs:456`), which is what actually reaches `SSAOp`'s `space` fields via
///   `SSAFunction::from_blocks_raw` (and therefore [`FunctionBehavior::from_blocks_raw`]).
/// - `"space_7"` — `r2ssa::block::space_name` (`r2ssa/src/block.rs:145-152`), a **separate**
///   conversion path this crate's own ingest does not currently exercise, handled here anyway
///   because nothing prevents a caller from handing this function a string produced by it.
///
/// ⚠ **MEASUREMENT ONLY — never a data path.** The plan forbids parsing display strings for
/// behavioral truth (§2's honesty notes); this function exists solely so [`VocabStats`] can put a
/// NUMBER on the loss by comparing its output against the TYPED oracle,
/// [`custom_space_ids_from_blocks`]. A `None` here means only that this particular string did not
/// match either known shape — it must never gate behavior, and no other module in this crate may
/// call it as part of ore/furnace/slag processing.
///
/// [`FunctionBehavior::from_blocks_raw`]: crate::behavior::FunctionBehavior::from_blocks_raw
pub fn parse_custom_space_id(space: &str) -> Option<u32> {
    if let Some(digits) = space
        .strip_prefix("Custom(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return digits.parse().ok();
    }
    space
        .strip_prefix("space_")
        .and_then(|digits| digits.parse().ok())
}

/// Recover a possible custom-space ordinal from an upstream **`SSAVar::name`**.
///
/// Named variables for a `SpaceId::Custom(n)` varnode are built by `r2ssa::naming::varnode_to_name`
/// (`r2ssa/src/naming.rs:133`) as `format!("space{}:{:x}", id, offset)` — e.g. `"space7:20"` for
/// `Custom(7)` at offset `0x20`: no underscore between `space` and the id, offset in hex after the
/// colon, no version suffix (that is `SSAVar::display_name`, a different string this function does
/// not parse). This is a genuinely different shape from [`parse_custom_space_id`]'s inputs, which
/// is why the two are separate functions rather than one shared parser guessing at a shape — each
/// names its one real upstream source.
///
/// ⚠ **MEASUREMENT ONLY — never a data path.** Same caveat as [`parse_custom_space_id`]: this
/// exists to measure loss against the typed oracle, [`custom_space_ids_from_blocks`], never to
/// drive behavior.
pub fn custom_space_id_from_var_name(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("space")?;
    let digits = rest.split(':').next()?;
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2il::{MemoryOrdering, Varnode};

    fn reg(offset: u64, size: u32) -> Varnode {
        Varnode::register(offset, size)
    }

    /// 1. `vocab_table_is_order_independent` — the same 5 names in two orders (one with
    ///    duplicates) build equal tables; anti-vacuity: `len() == 5` and at least two ids differ.
    #[test]
    fn vocab_table_is_order_independent() {
        let ordered = VocabTable::from_names(["alpha", "beta", "gamma", "delta", "epsilon"]);
        let scrambled_with_dupes = VocabTable::from_names([
            "epsilon", "delta", "delta", "gamma", "beta", "alpha", "alpha",
        ]);
        assert_eq!(ordered, scrambled_with_dupes);

        // Anti-vacuity: a `from_names` that mapped every name to the same id (or dropped
        // duplicates into one bucket by accident) would still satisfy the equality above.
        assert_eq!(ordered.len(), 5);
        let alpha = ordered.id_of("alpha").expect("alpha was interned");
        let beta = ordered.id_of("beta").expect("beta was interned");
        assert_ne!(alpha, beta);
    }

    /// 2. `harvest_counts_every_userop_mention_but_interns_names_once` — two `CallOther`s sharing
    ///    `userop: 7` plus one with `9` → `unique_userops == 2`, `userop_mentions == 3` (both
    ///    exact), reused name interned once.
    ///
    /// The fixture: a single `Return`-terminated block (proven ingestible in isolation by
    /// `behavior::tests::empty_block_list_is_none_not_a_panic`) whose first two `CallOther`s both
    /// *write* `reg:38` (two distinct SSA versions, same physical register — the "reused name"),
    /// and all three `CallOther`s *read* `reg:30` (one SSA version, read three times).
    #[test]
    fn harvest_counts_every_userop_mention_but_interns_names_once() {
        let mut block = R2ILBlock::new(0x2000, 4);
        block.push(R2ILOp::CallOther {
            output: Some(reg(0x38, 8)),
            userop: 7,
            inputs: vec![reg(0x30, 8)],
        });
        block.push(R2ILOp::CallOther {
            output: Some(reg(0x38, 8)),
            userop: 7,
            inputs: vec![reg(0x30, 8)],
        });
        block.push(R2ILOp::CallOther {
            output: None,
            userop: 9,
            inputs: vec![reg(0x30, 8)],
        });
        block.push(R2ILOp::Return {
            target: reg(0x38, 8),
        });

        let behavior = FunctionBehavior::from_blocks_raw(&[block], None)
            .expect("a single Return-terminated block ingests");
        let harvest = VocabHarvest::from_behavior(&behavior);
        let stats = harvest.stats();

        assert_eq!(stats.unique_userops, 2, "userop ids 7 and 9, no more");
        assert_eq!(stats.userop_mentions, 3, "two CallOthers on 7, one on 9");

        // "reused name interned once": reg:38 is DEFINED twice (two SSA versions, so it
        // contributes 2 to the raw `total_values` count) yet only two distinct NAME strings
        // ("reg:30", "reg:38") ever existed across the whole function.
        assert_eq!(harvest.ssa_names.len(), 2);
        assert!(harvest.ssa_names.id_of("reg:38").is_some());
        assert!(harvest.ssa_names.id_of("reg:30").is_some());
        assert_eq!(
            stats.total_values, 3,
            "reg:30 v0 (1 occurrence) + reg:38 v0,v1 (2 occurrences) = 3 raw values over 2 names"
        );
    }

    /// 3. (recommended) `typed_custom_space_ids_are_the_oracle_for_the_string_set` —
    ///    `custom_space_ids_from_blocks == {7}`; the string-recovered set equals it on this
    ///    fixture, with a doc comment recording that the equality is a measurement, not a
    ///    guarantee.
    #[test]
    fn typed_custom_space_ids_are_the_oracle_for_the_string_set() {
        let mut block = R2ILBlock::new(0x1010, 4);
        block.push(R2ILOp::LoadGuarded {
            dst: reg(0x30, 8),
            space: SpaceId::Custom(7),
            addr: reg(0x20, 8),
            guard: reg(0x28, 1),
            ordering: MemoryOrdering::Acquire,
        });
        block.push(R2ILOp::Return {
            target: reg(0x30, 8),
        });
        let blocks = vec![block];

        let typed = custom_space_ids_from_blocks(&blocks);
        assert_eq!(typed, BTreeSet::from([7]));

        // The string-recovered set, built from the EXACT upstream string shapes this session
        // verified by reading source (see `parse_custom_space_id` / `custom_space_id_from_var_name`
        // doc comments for the cited call sites) — NOT by running the rename pass here:
        //   - rename.rs:456 stamps an `SSAOp::LoadGuarded`'s `space: String` with
        //     `format!("{:?}", SpaceId::Custom(7))` -> `"Custom(7)"`.
        //   - naming.rs:133 names the `Custom(7)` varnode at offset `0x20` as
        //     `format!("space{}:{:x}", 7, 0x20u64)` -> `"space7:20"`.
        //
        // Equality below is a MEASUREMENT on this fixture's two known shapes, never a guarantee
        // that every upstream space string is recoverable this way.
        let mut recovered = BTreeSet::new();
        if let Some(id) = parse_custom_space_id("Custom(7)") {
            recovered.insert(id);
        }
        if let Some(id) = custom_space_id_from_var_name("space7:20") {
            recovered.insert(id);
        }
        assert_eq!(recovered, typed);

        // Anti-vacuity for the string parsers themselves: an unrelated string must not resolve.
        assert_eq!(parse_custom_space_id("Ram"), None);
        assert_eq!(custom_space_id_from_var_name("reg:38"), None);
    }
}
