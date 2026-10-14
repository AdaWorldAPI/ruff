//! Recovering the absolute address a `Load`/`Store` actually names.
//!
//! # The defect this exists to fix
//!
//! `reachability_probe` collected static data references with the filter
//! `addr.space == SpaceId::Const && in_image(addr.offset)`, and its `unreached+ref` column
//! was therefore **structurally zero** — not measured-zero. A full linear sweep of
//! `LOCODE.unprot.bin` counts 1418 `Load`/`Store` address operands: 860 in `Unique`, 328 in
//! `Register`, 230 in `Const`, and **not one `Const` operand inside the image**. A column
//! that cannot be non-zero is not a measurement, and the fix is not to widen the filter
//! until something matches.
//!
//! # What the lifted 6502 actually looks like
//!
//! Absolute-indexed addressing does not put the address in the address operand. Measured at
//! `$1d27` (a 3-byte `STA $ce00,y`):
//!
//! ```text
//! IntZExt { dst: Unique 7680, src: Register 1 }                    ; widen the index
//! IntAdd  { dst: Unique 7936, a: Const 52736, b: Unique 7680 }     ; base + index
//! Copy    { dst: Unique 8064, src: Register 0 }
//! Store   { addr: Unique 7936, val: Unique 8064 }                  ; <- address is a TEMP
//! ```
//!
//! The data address is `Const 52736` = `$ce00`, the constant operand of the `IntAdd` that
//! defines the address temp. It is never the address operand itself.
//!
//! The near-identical shape at `$1d2d` is the one that must NOT count:
//!
//! ```text
//! IntSub { dst: Unique 19584, a: Register 34, b: Const 1 }         ; stack pointer - 1
//! Store  { addr: Unique 19584, val: Const 7472 }                   ; a stack push
//! ```
//!
//! Same `Store`-through-a-temp silhouette, and its constant (`1`) is an offset from a
//! register rather than a base address. Emitting `$0001` as a data reference here would be
//! worse than emitting nothing, because it would look like a measurement.
//!
//! # The rule, and its deliberate limit
//!
//! For each `Load`/`Store`, resolve its address operand:
//!
//! - `Const` — the address, directly. (Zero page and the hardware vectors arrive this way.)
//! - `Unique` — find the **nearest preceding op that writes that temp**, via
//!   [`R2ILOp::output`], and accept only an `IntAdd` carrying a constant. Nearest-preceding
//!   is what makes this correct rather than approximate: 6502 blocks reuse temp offsets
//!   freely (offset 1920 is reassigned nine times in one BRK block), so a scan that took
//!   the *first* definition would read a stale base.
//! - anything else — no address. A `Register` operand is indirect addressing and is not
//!   resolvable without a value analysis; guessing is not on offer.
//!
//! This is one level deep on purpose. A `Copy` chain into the address temp would also be
//! resolvable and is cheap to add — it is absent because it was not observed on this
//! corpus, and untested code that looks like it works is worse than an honest gap.

use std::collections::BTreeSet;

use r2il::{R2ILOp, SpaceId, Varnode};

/// The constant operand of an `IntAdd`, if it has exactly one — or the sum if both are
/// constant.
fn add_base(a: &Varnode, b: &Varnode) -> Option<u64> {
    match (a.space, b.space) {
        // A fully constant address: rare, but it is still an address.
        (SpaceId::Const, SpaceId::Const) => Some(a.offset.wrapping_add(b.offset)),
        (SpaceId::Const, _) => Some(a.offset),
        (_, SpaceId::Const) => Some(b.offset),
        _ => None,
    }
}

/// Resolve one address operand against the ops that precede it in the same block.
///
/// `at` is the index of the `Load`/`Store` itself; only ops before it are consulted, so a
/// later redefinition of the same temp cannot leak backwards.
#[must_use]
pub fn resolve_address(ops: &[R2ILOp], at: usize, addr: &Varnode) -> Option<u64> {
    match addr.space {
        SpaceId::Const => Some(addr.offset),
        SpaceId::Unique => {
            // Nearest preceding writer of this temp — see the module docs on reuse.
            let defining = ops[..at.min(ops.len())].iter().rev().find(|op| {
                op.output()
                    .is_some_and(|o| o.space == SpaceId::Unique && o.offset == addr.offset)
            })?;
            match defining {
                R2ILOp::IntAdd { a, b, .. } => add_base(a, b),
                // Every other definer — IntSub from a register (the stack), a shift, a
                // load — leaves an address this pass cannot claim to know.
                _ => None,
            }
        }
        _ => None,
    }
}

/// Every distinct absolute address the block's loads and stores name.
///
/// Callers decide what "in image" means; this returns addresses, not judgements.
#[must_use]
pub fn absolute_refs(ops: &[R2ILOp]) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for (i, op) in ops.iter().enumerate() {
        let addr = match op {
            R2ILOp::Load { addr, .. } | R2ILOp::Store { addr, .. } => addr,
            _ => continue,
        };
        if let Some(a) = resolve_address(ops, i, addr) {
            out.insert(a);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vn(space: SpaceId, offset: u64, size: u32) -> Varnode {
        Varnode {
            space,
            offset,
            size,
            meta: None,
        }
    }

    /// The measured `STA $ce00,y` at `$1d27`, op for op.
    fn indexed_absolute_store() -> Vec<R2ILOp> {
        vec![
            R2ILOp::IntZExt {
                dst: vn(SpaceId::Unique, 7680, 2),
                src: vn(SpaceId::Register, 1, 1),
            },
            R2ILOp::IntAdd {
                dst: vn(SpaceId::Unique, 7936, 2),
                a: vn(SpaceId::Const, 0xce00, 2),
                b: vn(SpaceId::Unique, 7680, 2),
            },
            R2ILOp::Copy {
                dst: vn(SpaceId::Unique, 8064, 1),
                src: vn(SpaceId::Register, 0, 1),
            },
            R2ILOp::Store {
                space: SpaceId::Ram,
                addr: vn(SpaceId::Unique, 7936, 2),
                val: vn(SpaceId::Unique, 8064, 1),
            },
        ]
    }

    /// The measured stack push at `$1d2d` — the same silhouette, and not a data reference.
    fn stack_push() -> Vec<R2ILOp> {
        vec![
            R2ILOp::IntSub {
                dst: vn(SpaceId::Unique, 19584, 2),
                a: vn(SpaceId::Register, 34, 2),
                b: vn(SpaceId::Const, 1, 2),
            },
            R2ILOp::Store {
                space: SpaceId::Ram,
                addr: vn(SpaceId::Unique, 19584, 2),
                val: vn(SpaceId::Const, 7472, 2),
            },
        ]
    }

    #[test]
    fn an_indexed_absolute_store_resolves_to_its_base() {
        let refs = absolute_refs(&indexed_absolute_store());
        assert_eq!(refs, BTreeSet::from([0xce00]));
    }

    #[test]
    fn a_stack_push_resolves_to_nothing() {
        // The silence half, and it is the one that matters: this block DOES carry a
        // constant (1), and a pass that reached for "the constant near the store" would
        // emit $0001 as a data address. Both halves run against real measured shapes, so
        // neither can pass by being trivially empty.
        assert!(
            absolute_refs(&stack_push()).is_empty(),
            "an offset from the stack pointer is not a data address"
        );
        assert!(!absolute_refs(&indexed_absolute_store()).is_empty());
    }

    #[test]
    fn a_direct_constant_address_resolves() {
        // Zero page and the hardware vectors arrive this way; $fffe is the IRQ vector,
        // observed verbatim in a real BRK block.
        let ops = vec![R2ILOp::Load {
            dst: vn(SpaceId::Unique, 16512, 2),
            space: SpaceId::Ram,
            addr: vn(SpaceId::Const, 0xfffe, 2),
        }];
        assert_eq!(absolute_refs(&ops), BTreeSet::from([0xfffe]));
    }

    #[test]
    fn a_redefined_temp_reads_its_nearest_definition_not_a_stale_one() {
        // 6502 blocks reuse temp offsets freely. If the scan took the FIRST definition
        // instead of the nearest preceding one, this would resolve to $ce00 — a real
        // address, from the wrong instruction.
        let mut ops = indexed_absolute_store();
        ops.insert(
            3,
            R2ILOp::IntAdd {
                dst: vn(SpaceId::Unique, 7936, 2),
                a: vn(SpaceId::Const, 0x1234, 2),
                b: vn(SpaceId::Unique, 7680, 2),
            },
        );
        assert_eq!(absolute_refs(&ops), BTreeSet::from([0x1234]));
    }

    #[test]
    fn an_undefined_temp_resolves_to_nothing() {
        let ops = vec![R2ILOp::Store {
            space: SpaceId::Ram,
            addr: vn(SpaceId::Unique, 999, 2),
            val: vn(SpaceId::Const, 0, 1),
        }];
        assert!(absolute_refs(&ops).is_empty());
    }

    #[test]
    fn an_indirect_register_address_is_not_guessed_at() {
        // Indirect addressing needs a value analysis. Returning nothing is the honest
        // answer; returning the register number would be nonsense that looks like data.
        let ops = vec![R2ILOp::Load {
            dst: vn(SpaceId::Register, 0, 1),
            space: SpaceId::Ram,
            addr: vn(SpaceId::Register, 34, 2),
        }];
        assert!(absolute_refs(&ops).is_empty());
    }
}
