//! The longest-prefix-wins config tree over varnode identity space.
//!
//! Precedent: `ruff_spo_triplet::concept_split::ConceptConvention` (caller-supplied, **zero
//! domain vocabulary in the module**) and OGAR codebook scoping — *"longest-prefix wins — one
//! rule, every level."*
//! **This module ships ZERO architecture vocabulary.** No register names, no opcode semantics,
//! no userop table are hardcoded anywhere below. Everything the convention knows arrives as
//! data, either bootstrapped by reading it off an [`ArchSpec`] ([`R2ilConvention::from_arch`])
//! or inserted by a caller ([`R2ilConvention::insert`]).
//!
//! # Not a flat table
//!
//! [`R2ilConvention`] is a radix tree over [`VarnodeFacet`] space, keyed by [`FacetPrefix`].
//! Rows attach at one of three prefix depths — space-class alone, space-class+offset, or the
//! full space-class+offset+size — and [`R2ilConvention::resolve`] walks from the finest prefix
//! down to the coarsest, returning the first row it finds (**longest matching prefix wins**).
//! An address the convention says nothing about resolves to `None`, which is exactly the
//! addressed-residual case `slag.rs` names: the proposer emits a proposed [`ConventionRow`] AT
//! that address and the next drill pass melts it — the config accumulates as a radix tree,
//! self-scaffolding, one row at a time, never a code edit to a match arm.
//!
//! # The slag doctrine, restated for this module
//!
//! *"The residual is not waste: it is the empirical boundary of the current convention. A
//! recurring residual reason names the next convention fact to add."* This module is the thing
//! that boundary is measured against — every [`R2ilConvention::resolve`] miss is a fact about
//! what the convention does not yet know, not a defect in this module.

use std::collections::{BTreeMap, BTreeSet};

use r2il::{ArchSpec, SpaceId};

use crate::facet::{CustomSpaceTable, FacetOverflow, FacetPrefix, SPACE_REGISTER, VarnodeFacet};
use crate::ore::OpTag;

/// Mirrors openproject-nexgen-rs's `orm-ar-backprojection.toml`
/// (`validation_states = [unmeasured|confirmed|corrected|retired]`, meta key
/// `measure_dont_claim`). **Every row starts `Unmeasured`** — a convention row records where
/// the drill believes an address resolves, never a claim that the belief was checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationState {
    Unmeasured,
    Confirmed,
    Corrected,
    Retired,
}

impl ValidationState {
    /// The stable, lowercase name used by [`R2ilConvention::to_toml`]. Never derived from
    /// `Debug` — a rename of the variant must not silently change the emitted TOML.
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationState::Unmeasured => "unmeasured",
            ValidationState::Confirmed => "confirmed",
            ValidationState::Corrected => "corrected",
            ValidationState::Retired => "retired",
        }
    }
}

/// One config row, attached at a facet PREFIX.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConventionRow {
    pub at: FacetPrefix,
    /// e.g. a register name, a space name. `None` for a row the proposer attached before it
    /// had a better label than the address itself.
    pub name: Option<String>,
    /// Free provenance text; never branched on — this is a note for a human, not a fact the
    /// resolver reads.
    pub note: Option<String>,
    pub state: ValidationState,
}

/// The drilling convention: a radix tree over varnode identity space.
///
/// See the module docs for the "not a flat table" / "longest matching prefix wins" framing.
#[derive(Debug, Clone, Default)]
pub struct R2ilConvention {
    /// `BTreeMap` ⇒ deterministic emission order, both for `rows()` and for `to_toml()`.
    rows: BTreeMap<FacetPrefix, ConventionRow>,
    spaces: CustomSpaceTable,
    userops: BTreeMap<u32, String>,
    /// THE furnace ladder, as DATA. Pass 1 carries exactly seven entries. Widening the pass is
    /// a CONFIG change with a measured before/after ledger — never a code edit to a match arm.
    classified_opcodes: BTreeSet<OpTag>,
    /// Provenance only; never branched on.
    pub arch: Option<String>,
}

impl R2ilConvention {
    /// Pass 1: `[Copy, IntAdd, Load, Store, CBranch, Call, Return]`, no rows, no userops, no
    /// custom spaces. Deliberately minimal — the stressors in `tests/lossless_fixtures.rs`
    /// MUST land in slag under this convention, and the ledger NAMING them is the acceptance
    /// criterion, not a bigger match.
    pub fn minimal_pass_one() -> Self {
        Self {
            rows: BTreeMap::new(),
            spaces: CustomSpaceTable::default(),
            userops: BTreeMap::new(),
            classified_opcodes: BTreeSet::from([
                OpTag::Copy,
                OpTag::IntAdd,
                OpTag::Load,
                OpTag::Store,
                OpTag::CBranch,
                OpTag::Call,
                OpTag::Return,
            ]),
            arch: None,
        }
    }

    /// BOOTSTRAP — read, never retype. Populates from data that already exists on `ArchSpec`:
    ///
    /// * `arch.registers: Vec<RegisterDef{name, offset, size, parent}>` → one
    ///   `FacetPrefix::SpaceOffsetSize{ SPACE_REGISTER, offset, size }` row per register,
    ///   `name = Some(reg.name)`, `state = Unmeasured`;
    /// * one coarse `FacetPrefix::Space{ SPACE_REGISTER }` fall-through row named after the
    ///   register space (`arch.spaces`'s own `AddressSpace{id: SpaceId::Register, name}` entry
    ///   when the architecture defines one, else the literal `"register"`), so an UNKNOWN
    ///   register offset still resolves — to the space, not to nothing;
    /// * `arch.userops: Vec<UserOpDef{index, name}>` → the userop table;
    /// * `arch.spaces` (`AddressSpace{id: SpaceId::Custom(n), name}`) → [`CustomSpaceTable`].
    ///
    /// Errors only through [`FacetOverflow`] — config keys must be lossless (see `facet.rs`).
    pub fn from_arch(
        arch: &ArchSpec,
        classified: impl IntoIterator<Item = OpTag>,
    ) -> Result<Self, FacetOverflow> {
        let spaces = CustomSpaceTable::from_arch(arch)?;

        let mut rows = BTreeMap::new();

        // The coarse fall-through row for the whole register space — an offset the corpus
        // never named still resolves to *something* rather than to nothing.
        let register_space_name = arch
            .spaces
            .iter()
            .find(|space| space.id == SpaceId::Register)
            .map(|space| space.name.clone())
            .unwrap_or_else(|| "register".to_string());
        let register_space_prefix = FacetPrefix::Space {
            discriminant: SPACE_REGISTER,
        };
        rows.insert(
            register_space_prefix,
            ConventionRow {
                at: register_space_prefix,
                name: Some(register_space_name),
                note: None,
                state: ValidationState::Unmeasured,
            },
        );

        // One finest-depth row per named register, read straight off `ArchSpec::registers`.
        for register in &arch.registers {
            let at = FacetPrefix::SpaceOffsetSize {
                discriminant: SPACE_REGISTER,
                offset: register.offset,
                size: register.size,
            };
            rows.insert(
                at,
                ConventionRow {
                    at,
                    name: Some(register.name.clone()),
                    note: None,
                    state: ValidationState::Unmeasured,
                },
            );
        }

        let mut userops = BTreeMap::new();
        for userop in &arch.userops {
            userops.insert(userop.index, userop.name.clone());
        }

        Ok(Self {
            rows,
            spaces,
            userops,
            classified_opcodes: classified.into_iter().collect(),
            arch: Some(arch.name.clone()),
        })
    }

    /// Longest-prefix-wins resolution: try `SpaceOffsetSize`, then `SpaceOffset`, then `Space`;
    /// first hit wins. `None` means the convention says nothing at this address — an addressed
    /// residual for `slag.rs`.
    pub fn resolve(&self, facet: &VarnodeFacet) -> Option<&ConventionRow> {
        facet
            .prefixes()
            .into_iter()
            .rev()
            .find_map(|prefix| self.rows.get(&prefix))
    }

    /// The longest prefix that DID resolve — what an addressed residual reports so the
    /// proposer knows where to attach the next, finer row.
    pub fn resolved_prefix(&self, facet: &VarnodeFacet) -> Option<FacetPrefix> {
        facet
            .prefixes()
            .into_iter()
            .rev()
            .find(|prefix| self.rows.contains_key(prefix))
    }

    pub fn classifies(&self, op: OpTag) -> bool {
        self.classified_opcodes.contains(&op)
    }

    pub fn userop_name(&self, index: u32) -> Option<&str> {
        self.userops.get(&index).map(String::as_str)
    }

    pub fn spaces(&self) -> &CustomSpaceTable {
        &self.spaces
    }

    /// The proposer entry point: attach (or overwrite) a row at its own `at` prefix.
    pub fn insert(&mut self, row: ConventionRow) {
        self.rows.insert(row.at, row);
    }

    /// `BTreeMap` order — deterministic, matching `to_toml`'s row order.
    pub fn rows(&self) -> impl Iterator<Item = &ConventionRow> {
        self.rows.values()
    }

    /// Nested-TOML rendering, mirroring the harvest precedents. Hand-written (no serde dep):
    /// a `[meta]` table (`measure_dont_claim`, `validation_states`, `arch`,
    /// `classified_opcodes`), then one `[[row]]` per row in `BTreeMap` order with
    /// `prefix_depth`, `space`, `offset`, `size`, `name`, `state`. **Emission only** — nothing
    /// in this crate (or in ruff) parses this back.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();

        out.push_str("[meta]\n");
        out.push_str("measure_dont_claim = true\n");
        out.push_str(
            "validation_states = [\"unmeasured\", \"confirmed\", \"corrected\", \"retired\"]\n",
        );
        if let Some(arch) = &self.arch {
            out.push_str("arch = ");
            out.push_str(&toml_quote(arch));
            out.push('\n');
        }
        out.push_str("classified_opcodes = [");
        for (index, op) in self.classified_opcodes.iter().enumerate() {
            if index > 0 {
                out.push_str(", ");
            }
            out.push_str(&toml_quote(op.as_str()));
        }
        out.push_str("]\n");

        for row in self.rows.values() {
            push_row_toml(&mut out, row);
        }

        out
    }
}

/// Render one `[[row]]` table. `prefix_depth`/`space`/`offset`/`size` follow the prefix's own
/// depth — a `Space` row has no `offset`/`size` key at all rather than a fabricated `0`.
fn push_row_toml(out: &mut String, row: &ConventionRow) {
    out.push_str("\n[[row]]\n");
    match row.at {
        FacetPrefix::Space { discriminant } => {
            out.push_str("prefix_depth = 1\n");
            out.push_str(&format!("space = {discriminant}\n"));
        }
        FacetPrefix::SpaceOffset {
            discriminant,
            offset,
        } => {
            out.push_str("prefix_depth = 2\n");
            out.push_str(&format!("space = {discriminant}\n"));
            out.push_str(&format!("offset = {offset}\n"));
        }
        FacetPrefix::SpaceOffsetSize {
            discriminant,
            offset,
            size,
        } => {
            out.push_str("prefix_depth = 3\n");
            out.push_str(&format!("space = {discriminant}\n"));
            out.push_str(&format!("offset = {offset}\n"));
            out.push_str(&format!("size = {size}\n"));
        }
    }
    if let Some(name) = &row.name {
        out.push_str("name = ");
        out.push_str(&toml_quote(name));
        out.push('\n');
    }
    if let Some(note) = &row.note {
        out.push_str("note = ");
        out.push_str(&toml_quote(note));
        out.push('\n');
    }
    out.push_str(&format!("state = \"{}\"\n", row.state.as_str()));
}

/// Minimal TOML basic-string quoting (`"`, `\`, and `\n` escaped). Emission-only, matching
/// `to_toml`'s own contract — this never parses TOML back.
fn toml_quote(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for ch in value.chars() {
        match ch {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facet::project;
    use r2il::serialize::UserOpDef;
    use r2il::{AddressSpace, RegisterDef, Varnode};

    #[test]
    fn arch_registers_bootstrap_the_register_branch() {
        let mut arch = ArchSpec::new("test-arch");
        // Two registers share an offset and differ only in size — exactly why the finest
        // prefix must carry size, not just space+offset.
        arch.registers = vec![
            RegisterDef::new("rax", 0, 8),
            RegisterDef::new("eax", 0, 4),
            RegisterDef::new("rbx", 8, 8),
        ];
        let conv = R2ilConvention::from_arch(&arch, std::iter::empty())
            .expect("no custom spaces on this arch, must not overflow");

        let mut resolved_names = Vec::new();
        for register in &arch.registers {
            let vn = Varnode::register(register.offset, register.size);
            let facet = project(&vn, conv.spaces()).expect("register varnode must project");
            let row = conv
                .resolve(&facet)
                .expect("a bootstrapped register must resolve");
            assert_eq!(row.state, ValidationState::Unmeasured);
            let name = row
                .name
                .as_deref()
                .expect("a bootstrapped register row must carry a name");
            assert_eq!(name, register.name);
            resolved_names.push(name.to_string());
        }

        // Anti-vacuity: the three resolved names must be DISTINCT — a bootstrap that mapped
        // every register onto one shared row would otherwise still pass the loop above.
        let mut distinct = resolved_names.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            3,
            "expected 3 distinct register names, got {resolved_names:?}"
        );
    }

    #[test]
    fn an_unknown_register_offset_falls_through_to_the_space_prefix() {
        let mut arch = ArchSpec::new("test-arch");
        arch.registers = vec![RegisterDef::new("rax", 0, 8)];
        let conv = R2ilConvention::from_arch(&arch, std::iter::empty())
            .expect("no custom spaces, must not overflow");

        // Two-sided: a KNOWN register resolves at the finest depth.
        let known = Varnode::register(0, 8);
        let known_facet = project(&known, conv.spaces()).expect("must project");
        assert_eq!(
            conv.resolved_prefix(&known_facet)
                .map(|prefix| prefix.depth()),
            Some(3),
            "a known register must resolve to the depth-3 row"
        );

        // An UNKNOWN register offset falls through to the coarse Space row — not to `None`
        // and not to a depth-3 row.
        let unknown = Varnode::register(0xDEAD, 8);
        let unknown_facet = project(&unknown, conv.spaces()).expect("must project");
        let row = conv
            .resolve(&unknown_facet)
            .expect("an unknown register offset must still fall through to the space row");
        assert_eq!(row.state, ValidationState::Unmeasured);
        assert_eq!(
            conv.resolved_prefix(&unknown_facet)
                .map(|prefix| prefix.depth()),
            Some(1),
            "an unknown register offset must resolve at depth 1, not depth 3 and not None"
        );
    }

    #[test]
    fn longest_prefix_wins_over_a_coarser_row() {
        let mut conv = R2ilConvention::minimal_pass_one();
        let discriminant = SPACE_REGISTER;
        let offset: u64 = 0x10;
        let coarse_prefix = FacetPrefix::SpaceOffset {
            discriminant,
            offset,
        };
        let fine_prefix = FacetPrefix::SpaceOffsetSize {
            discriminant,
            offset,
            size: 8,
        };
        conv.insert(ConventionRow {
            at: coarse_prefix,
            name: Some("coarse".to_string()),
            note: None,
            state: ValidationState::Unmeasured,
        });
        conv.insert(ConventionRow {
            at: fine_prefix,
            name: Some("fine".to_string()),
            note: None,
            state: ValidationState::Unmeasured,
        });

        // The facet whose size matches the finer row resolves to it, not to the coarser one.
        let matching_size = Varnode::register(offset, 8);
        let matching_facet = project(&matching_size, conv.spaces()).expect("must project");
        let matching_row = conv
            .resolve(&matching_facet)
            .expect("must resolve to the finer row");
        assert_eq!(matching_row.name.as_deref(), Some("fine"));

        // A facet at the SAME space+offset but a DIFFERENT size has no depth-3 row, so it
        // falls back to the coarser SpaceOffset row — falsifies a first-match-wins or
        // coarsest-wins implementation.
        let other_size = Varnode::register(offset, 4);
        let other_facet = project(&other_size, conv.spaces()).expect("must project");
        let other_row = conv
            .resolve(&other_facet)
            .expect("must resolve to the coarser row");
        assert_eq!(other_row.name.as_deref(), Some("coarse"));
    }

    #[test]
    fn custom_space_overflow_fails_at_config_key_time() {
        // A within-budget spec succeeds.
        let mut ok_arch = ArchSpec::new("ok-arch");
        ok_arch.spaces = (0..4u32)
            .map(|raw| AddressSpace::new(SpaceId::Custom(raw), format!("custom{raw}"), 8))
            .collect();
        assert!(R2ilConvention::from_arch(&ok_arch, std::iter::empty()).is_ok());

        // A spec whose custom spaces exceed the lo-u16 budget errors — typed, not a wrap.
        let over_budget = crate::facet::MAX_CUSTOM_ORDINAL as u32 + 2;
        let mut overflow_arch = ArchSpec::new("overflow-arch");
        overflow_arch.spaces = (0..over_budget)
            .map(|raw| AddressSpace::new(SpaceId::Custom(raw), format!("custom{raw}"), 8))
            .collect();
        let err = R2ilConvention::from_arch(&overflow_arch, std::iter::empty())
            .expect_err("a budget-exceeding custom space table must error, not truncate");
        assert!(
            matches!(err, FacetOverflow::CustomOrdinalExhausted { .. }),
            "expected CustomOrdinalExhausted, got {err:?}"
        );
    }

    #[test]
    fn toml_rendering_is_byte_stable_and_starts_every_row_unmeasured() {
        let mut arch = ArchSpec::new("test-arch");
        arch.registers = vec![RegisterDef::new("rax", 0, 8), RegisterDef::new("rbx", 8, 8)];
        arch.userops = vec![UserOpDef {
            index: 42,
            name: "syscall_helper".to_string(),
        }];
        let conv = R2ilConvention::from_arch(&arch, [OpTag::Copy, OpTag::IntAdd])
            .expect("no custom spaces, must not overflow");

        let rendered_once = conv.to_toml();
        let rendered_twice = conv.to_toml();
        assert_eq!(
            rendered_once, rendered_twice,
            "to_toml must be byte-stable across calls"
        );

        assert!(rendered_once.contains("measure_dont_claim = true"));
        assert!(rendered_once.contains("validation_states"));
        // Every row bootstrapped by from_arch starts Unmeasured — none of the other three
        // states appear anywhere in the render.
        assert!(rendered_once.contains("state = \"unmeasured\""));
        assert!(!rendered_once.contains("state = \"confirmed\""));
        assert!(!rendered_once.contains("state = \"corrected\""));
        assert!(!rendered_once.contains("state = \"retired\""));
    }
}
