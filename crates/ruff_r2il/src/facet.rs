//! The DRILL KEY: a 16-byte, V3-shaped, prefix-routable address over r2il varnode identity
//! space.
//!
//! **Role 1 (PR 1, load-bearing): the ADDRESS / CONFIG-KEY scheme.** [`VarnodeFacet`] is the
//! 16-byte V3-shaped identity `classid(space-class) | offset_lo | offset_hi | size`,
//! **prefix-routable by construction**. It is the key `convention.rs` drills on and the
//! coordinate `slag.rs` residuals are addressed at.
//!
//! **Role 2 (PR 2, NOT committed here): V3 SoA persistence.** Promoting the shape as a key
//! commits **no storage layout**. Same 16 bytes, two roles, no persistence decision yet.

use r2il::{ArchSpec, SpaceId, Varnode};

/// Provisional container concept. The REAL mint is a canon-high slot in
/// `lance_graph_contract::ogar_codebook` (plan PR 3, the `NETWORK_LAYER = 0x0804` analog).
/// Until then this is LOCAL and provisional — never persist it as an address.
///
/// ⚠ Known tension, recorded rather than hidden: OGAR's consumer rule is "hi u16 = shared
/// concept, lo u16 = APP render prefix — NEVER a shape ordinal", and the space discriminant
/// below IS a shape ordinal in the lo half. PR 3 owns the real carving.
pub const PROVISIONAL_R2IL_VARNODE: u16 = 0x0000;

/// Fixed-space discriminants, valid for every `VarnodeFacet` regardless of architecture.
pub const SPACE_RAM: u16 = 0;
pub const SPACE_REGISTER: u16 = 1;
pub const SPACE_UNIQUE: u16 = 2;
pub const SPACE_CONST: u16 = 3;

/// The first ordinal handed out to an interned `SpaceId::Custom(u32)` raw id.
pub const CUSTOM_ORDINAL_BASE: u16 = 4;

/// The maximum number of distinct custom spaces a single [`CustomSpaceTable`] can intern
/// without its highest ordinal (`CUSTOM_ORDINAL_BASE + count - 1`) overflowing the lo-u16
/// budget. 65531.
pub const MAX_CUSTOM_ORDINAL: u16 = u16::MAX - CUSTOM_ORDINAL_BASE;

/// The 16-byte, little-endian drill key over one [`Varnode`]'s identity.
///
/// Layout, all little-endian:
/// - `0..4`   `classid: u32` = `((PROVISIONAL_R2IL_VARNODE as u32) << 16) | space_discriminant`
/// - `4..8`   offset, low 32 bits
/// - `8..12`  offset, high 32 bits
/// - `12..16` `size: u32`
///
/// `Varnode::meta` is **documented-excluded** (advisory plane) — matching upstream, where
/// `Varnode`'s `PartialEq`/`Hash` already ignore it (`r2il/src/varnode.rs:149-163`). Two
/// varnodes that differ only in `meta` project to byte-identical facets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VarnodeFacet(pub [u8; 16]);

impl VarnodeFacet {
    /// The lo-u16 half of the classid word — the space-class discriminant.
    pub fn space_discriminant(&self) -> u16 {
        let classid = u32::from_le_bytes([self.0[0], self.0[1], self.0[2], self.0[3]]);
        (classid & 0xFFFF) as u16
    }

    /// The full 64-bit offset, recombined from the low/high 32-bit halves.
    pub fn offset(&self) -> u64 {
        let lo = u32::from_le_bytes([self.0[4], self.0[5], self.0[6], self.0[7]]) as u64;
        let hi = u32::from_le_bytes([self.0[8], self.0[9], self.0[10], self.0[11]]) as u64;
        lo | (hi << 32)
    }

    /// The varnode's size in bytes.
    pub fn size(&self) -> u32 {
        u32::from_le_bytes([self.0[12], self.0[13], self.0[14], self.0[15]])
    }

    /// The three prefix keys this facet resolves against, coarsest first: space-class alone,
    /// then space-class+offset, then the full space-class+offset+size.
    pub fn prefixes(&self) -> [FacetPrefix; 3] {
        let discriminant = self.space_discriminant();
        let offset = self.offset();
        let size = self.size();
        [
            FacetPrefix::Space { discriminant },
            FacetPrefix::SpaceOffset { discriminant, offset },
            FacetPrefix::SpaceOffsetSize { discriminant, offset, size },
        ]
    }
}

/// A config-tree key: a facet PREFIX. Ordered coarse → fine, exactly the three levels
/// [`R2ilConvention`] (`convention.rs`) drills on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FacetPrefix {
    Space { discriminant: u16 },
    SpaceOffset { discriminant: u16, offset: u64 },
    SpaceOffsetSize { discriminant: u16, offset: u64, size: u32 },
}

impl FacetPrefix {
    /// `1` for [`FacetPrefix::Space`], `2` for [`FacetPrefix::SpaceOffset`], `3` for
    /// [`FacetPrefix::SpaceOffsetSize`].
    pub fn depth(&self) -> u8 {
        match self {
            FacetPrefix::Space { .. } => 1,
            FacetPrefix::SpaceOffset { .. } => 2,
            FacetPrefix::SpaceOffsetSize { .. } => 3,
        }
    }
}

/// Why a [`VarnodeFacet`] could not be projected or unprojected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FacetOverflow {
    /// A `SpaceId::Custom(raw)` the table does not know. Projection REFUSES; it never falls
    /// back to `raw as u16` (`65541` and `5` both truncate to `5`, silently colliding two
    /// distinct architecture-defined spaces).
    UnknownCustomSpace { raw: u32 },
    /// The set of distinct custom-space raw ids exceeds [`MAX_CUSTOM_ORDINAL`]. Overflow is
    /// EVIDENCE a route needs factoring, never silent truncation (the `mint_factored`
    /// principle).
    CustomOrdinalExhausted { count: usize },
}

impl core::fmt::Display for FacetOverflow {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            FacetOverflow::UnknownCustomSpace { raw } => {
                write!(f, "custom space {raw} is not in the interned table")
            }
            FacetOverflow::CustomOrdinalExhausted { count } => {
                write!(
                    f,
                    "{count} custom spaces exceed the {MAX_CUSTOM_ORDINAL}-space lo-u16 budget"
                )
            }
        }
    }
}

impl core::error::Error for FacetOverflow {}

/// Deterministic interning of `SpaceId::Custom(u32)` raw ids → lo-u16 ordinals, in sorted
/// order.
///
/// Config-key-time losslessness (the promoted `Custom(u32)` falsifier): a `Custom` id that
/// overflows the interned-ordinal budget must fail **typed** at CONFIG-KEY construction, not
/// only at projection. A config tree keyed by a truncated address would silently attach rows
/// to the wrong varnode family — the worst possible failure for a drill scheme. Hence
/// [`CustomSpaceTable::from_ids`] / [`CustomSpaceTable::from_arch`] return `Result`, and
/// `convention.rs` propagates it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomSpaceTable {
    ids: Vec<u32>,
}

impl CustomSpaceTable {
    /// Intern a set of raw `Custom` space ids. Deduplicates and sorts first, so ordinal
    /// assignment is deterministic regardless of input order.
    ///
    /// `Err(FacetOverflow::CustomOrdinalExhausted)` when the deduplicated set exceeds
    /// [`MAX_CUSTOM_ORDINAL`] — the `mint_factored` principle: overflow is EVIDENCE a route
    /// needs factoring, never silent truncation.
    pub fn from_ids<I: IntoIterator<Item = u32>>(ids: I) -> Result<Self, FacetOverflow> {
        let mut ids: Vec<u32> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.len() > MAX_CUSTOM_ORDINAL as usize {
            return Err(FacetOverflow::CustomOrdinalExhausted { count: ids.len() });
        }
        Ok(Self { ids })
    }

    /// Bootstrap from upstream data that already exists (read, never retype): every
    /// [`r2il::AddressSpace`] in `arch.spaces` whose `id` is `SpaceId::Custom(n)`.
    pub fn from_arch(arch: &ArchSpec) -> Result<Self, FacetOverflow> {
        let ids = arch.spaces.iter().filter_map(|space| match space.id {
            SpaceId::Custom(raw) => Some(raw),
            _ => None,
        });
        Self::from_ids(ids)
    }

    /// The lo-u16 ordinal assigned to `raw`, or `None` if it was never interned.
    pub fn ordinal_of(&self, raw: u32) -> Option<u16> {
        self.ids
            .binary_search(&raw)
            .ok()
            .map(|pos| CUSTOM_ORDINAL_BASE + pos as u16)
    }

    /// The raw `Custom` id an interned ordinal came from, or `None` if `ordinal` is out of
    /// range (below [`CUSTOM_ORDINAL_BASE`] or past the interned set).
    pub fn raw_of(&self, ordinal: u16) -> Option<u32> {
        let pos = ordinal.checked_sub(CUSTOM_ORDINAL_BASE)?;
        self.ids.get(pos as usize).copied()
    }

    /// The number of distinct custom spaces interned.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// `true` when no custom spaces have been interned.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Build the drill key from the TYPED r2il varnode. This is the ONLY constructor — a facet is
/// never derived from an `SSAVar` (which has no offset and no `SpaceId`; see the crate's
/// upstream-facts table).
pub fn project(vn: &Varnode, spaces: &CustomSpaceTable) -> Result<VarnodeFacet, FacetOverflow> {
    let discriminant = match vn.space {
        SpaceId::Ram => SPACE_RAM,
        SpaceId::Register => SPACE_REGISTER,
        SpaceId::Unique => SPACE_UNIQUE,
        SpaceId::Const => SPACE_CONST,
        SpaceId::Custom(raw) => spaces
            .ordinal_of(raw)
            .ok_or(FacetOverflow::UnknownCustomSpace { raw })?,
    };
    let classid = ((PROVISIONAL_R2IL_VARNODE as u32) << 16) | discriminant as u32;
    let offset_lo = (vn.offset & 0xFFFF_FFFF) as u32;
    let offset_hi = (vn.offset >> 32) as u32;

    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&classid.to_le_bytes());
    bytes[4..8].copy_from_slice(&offset_lo.to_le_bytes());
    bytes[8..12].copy_from_slice(&offset_hi.to_le_bytes());
    bytes[12..16].copy_from_slice(&vn.size.to_le_bytes());
    Ok(VarnodeFacet(bytes))
}

/// Recover a `Varnode` from a drill key. The inverse of [`project`]; `meta` is always `None`
/// on the result (it was never part of the projection — see [`VarnodeFacet`]'s docs).
pub fn unproject(f: &VarnodeFacet, spaces: &CustomSpaceTable) -> Result<Varnode, FacetOverflow> {
    let discriminant = f.space_discriminant();
    let space = match discriminant {
        SPACE_RAM => SpaceId::Ram,
        SPACE_REGISTER => SpaceId::Register,
        SPACE_UNIQUE => SpaceId::Unique,
        SPACE_CONST => SpaceId::Const,
        other => {
            let raw = spaces
                .raw_of(other)
                .ok_or(FacetOverflow::UnknownCustomSpace { raw: other as u32 })?;
            SpaceId::Custom(raw)
        }
    };
    Ok(Varnode::new(space, f.offset(), f.size()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use r2il::{ScalarKind, VarnodeMetadata};

    #[test]
    fn fixed_spaces_round_trip_byte_for_byte() {
        let table = CustomSpaceTable::default();
        let cases = [
            Varnode::ram(0x1000, 8),
            Varnode::register(0x40, 4),
            Varnode::unique(0x7, 2),
            Varnode::constant(0xFF, 1),
        ];
        let mut classids = Vec::new();
        for vn in &cases {
            let facet = project(vn, &table).expect("fixed space must project");
            classids.push(u32::from_le_bytes([
                facet.0[0], facet.0[1], facet.0[2], facet.0[3],
            ]));
            let back = unproject(&facet, &table).expect("fixed space must unproject");
            assert_eq!(&back, vn);
        }
        // Anti-vacuity: a constant-returning `project` would produce four identical classids.
        let mut sorted = classids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            classids.len(),
            "the four fixed-space classid words must be pairwise distinct: {classids:?}"
        );
    }

    #[test]
    fn custom_space_within_budget_round_trips() {
        let table = CustomSpaceTable::from_ids([3, 7, 9]).expect("3 ids is within budget");
        let vn = Varnode::new(SpaceId::Custom(7), 0x20, 4);
        let facet = project(&vn, &table).expect("interned custom space must project");
        // Sorted order is [3, 7, 9]; 7 is at position 1.
        assert_eq!(facet.space_discriminant(), CUSTOM_ORDINAL_BASE + 1);
        let back = unproject(&facet, &table).expect("must unproject");
        assert_eq!(back, vn);
    }

    #[test]
    fn custom_space_outside_the_table_errors_and_never_truncates() {
        let empty = CustomSpaceTable::default();
        let vn = Varnode::new(SpaceId::Custom(5), 0, 1);
        assert_eq!(
            project(&vn, &empty),
            Err(FacetOverflow::UnknownCustomSpace { raw: 5 })
        );

        // 65541 = 5 + 65536: the exact pair a `raw as u16` cast would collide on.
        let table = CustomSpaceTable::from_ids([5, 65541]).expect("2 ids is within budget");
        let vn_small = Varnode::new(SpaceId::Custom(5), 0x10, 4);
        let vn_big = Varnode::new(SpaceId::Custom(65541), 0x10, 4);
        let f_small = project(&vn_small, &table).expect("project raw=5");
        let f_big = project(&vn_big, &table).expect("project raw=65541");
        assert_ne!(f_small, f_big, "65541 must not truncate onto 5's facet");
        assert_eq!(unproject(&f_small, &table).expect("unproject raw=5"), vn_small);
        assert_eq!(unproject(&f_big, &table).expect("unproject raw=65541"), vn_big);
    }

    #[test]
    fn too_many_custom_spaces_is_a_typed_overflow_not_a_wrap() {
        let budget = MAX_CUSTOM_ORDINAL as u32;

        let exactly_budget: Vec<u32> = (0..budget).collect();
        assert!(
            CustomSpaceTable::from_ids(exactly_budget).is_ok(),
            "exactly the budget must succeed"
        );

        let over_budget: Vec<u32> = (0..(budget + 2)).collect();
        match CustomSpaceTable::from_ids(over_budget) {
            Err(FacetOverflow::CustomOrdinalExhausted { count }) => {
                assert_eq!(count, (budget + 2) as usize);
            }
            other => panic!("expected a typed CustomOrdinalExhausted, got {other:?}"),
        }
    }

    #[test]
    fn offsets_above_u32_max_survive_the_lo_hi_split() {
        let table = CustomSpaceTable::default();
        let offset: u64 = 0x1234_5678_9ABC_DEF0;
        let vn = Varnode::ram(offset, 8);
        let facet = project(&vn, &table).expect("ram varnode must project");
        assert_eq!(facet.offset(), offset);

        // Anti-vacuity: the low 32-bit word alone must NOT already equal the full offset —
        // otherwise a lo-only implementation (silently dropping the high half) would still
        // pass the assertion above.
        let lo = u32::from_le_bytes([facet.0[4], facet.0[5], facet.0[6], facet.0[7]]) as u64;
        assert_ne!(lo, offset);

        let back = unproject(&facet, &table).expect("must unproject");
        assert_eq!(back, vn);
    }

    #[test]
    fn meta_is_excluded_from_the_projection() {
        // Mirrors r2il::Varnode, whose PartialEq/Hash already ignore `meta`
        // (r2il/src/varnode.rs:149-163) — the facet projection preserves that contract.
        let table = CustomSpaceTable::default();
        let plain = Varnode::register(0x18, 4);
        let with_meta = plain.clone().with_meta(VarnodeMetadata {
            scalar_kind: Some(ScalarKind::SignedInt),
            ..Default::default()
        });
        let f_plain = project(&plain, &table).expect("project plain");
        let f_meta = project(&with_meta, &table).expect("project with meta");
        assert_eq!(f_plain, f_meta, "meta must not change the projected bytes");
    }

    #[test]
    fn prefixes_are_ordered_coarse_to_fine_and_share_their_ancestors() {
        let table = CustomSpaceTable::default();
        let vn = Varnode::register(0x40, 8);
        let facet = project(&vn, &table).expect("project");
        let prefixes = facet.prefixes();
        assert_eq!(prefixes[0].depth(), 1);
        assert_eq!(prefixes[1].depth(), 2);
        assert_eq!(prefixes[2].depth(), 3);

        // Falsifies a prefix builder that ignores a component: a facet in the same space but
        // at a different offset must share the coarsest prefix and differ at the next one.
        let vn_other_offset = Varnode::register(0x48, 8);
        let facet_other = project(&vn_other_offset, &table).expect("project other offset");
        let prefixes_other = facet_other.prefixes();
        assert_eq!(prefixes[0], prefixes_other[0]);
        assert_ne!(prefixes[1], prefixes_other[1]);
    }
}
