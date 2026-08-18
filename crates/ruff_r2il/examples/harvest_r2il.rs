//! THE R2IL pass-1 harvest — the deliverable artifact set (impl-spec §11).
//!
//! Lifts the same small, fixed x86-64 ELF64 corpus `r2il_corpus_profile.rs` (§12) profiles —
//! `r2sleigh/tests/e2e/{stress_test,stress_test_opt}` (symtab present) plus `/bin/ls` and
//! `/usr/bin/env` (stripped, function-level harvest skipped with a printed note) — but instead of
//! printing a profile, runs the real pipeline **per function**: a minimal ELF64 section/symtab
//! walk locates `STT_FUNC` boundaries, `Disassembler::lift`/`lift_block` (16-byte padded windows)
//! rebuilds basic blocks, `FunctionBehavior::from_blocks_raw` ingests them, and
//! `furnace::smelt(&behavior, &blocks, &conv)` melts them against ONE `R2ilConvention::from_arch`
//! built from the real x86-64 `ArchSpec` and the pass-1 seven (`Copy, IntAdd, Load, Store,
//! CBranch, Call, Return`). Results accumulate across every harvested function into six artifacts
//! written to `.claude/harvest/r2il/` (override via `R2IL_HARVEST_OUT`):
//!
//! - `r2il-pass1.ore.tsv`      — one row per melted `FlatFact`, `smelt` order, `at` as 32 hex chars
//! - `r2il-pass1-census.md`    — `furnace::census()` by fact kind / by opcode (`BTreeMap` order)
//! - `r2il-pass1-slag.tsv` — `ResidualLedger::grouped()` + `by_address()`, merged across
//!   every harvested function
//! - `r2il-convention.toml`    — `R2ilConvention::to_toml()`, the one convention every function used
//! - `PROVENANCE.md`           — corpus manifest (FNV-1a 64, not a sha), commit pin, env caps
//! - `TRIAGE-RESULT.md` — the three pre-registered bars B1/B2/B3, stated BEFORE the measured
//!   section, plus the non-bar 60-80% Op-classify prediction
//!
//! **These artifacts are evidence, never a re-ingest path — nothing in ruff parses them back.**
//!
//! No `unwrap`/`panic` on corpus input: every ELF field read is bounds-checked and returns
//! `Option`; a malformed, unreadable, non-x86-64, or stripped binary is skipped with a printed
//! note, never a crash.
//!
//! Run:
//! ```sh
//! cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example harvest_r2il
//! ```

// The workspace `clippy.toml` disallows `std::fs::*` and `std::env::var` with the reason "Use
// System::… instead **in ty crates**" — that policy exists so ty routes filesystem access through
// its `System` virtual-filesystem abstraction for testability. This is not a ty crate: `ruff_r2il`
// is workspace-EXCLUDED and does not (and should not) depend on `ty`, so `System` is unreachable
// here. The writes below are the deliverable itself — real harvest artifacts on real disk — and
// the env reads are the documented `R2IL_HARVEST_*` overrides. Suppressed narrowly and visibly
// (`expect`, per the repo's "prefer expect over allow" rule) rather than evaded by reaching for a
// non-disallowed API, which would hide that this crate sits outside the policy.
#![expect(
    clippy::disallowed_methods,
    reason = "not a ty crate: `System` is unavailable to this workspace-excluded crate, and these are the example's real artifact writes and documented env overrides"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::PathBuf;

use r2il::{ArchSpec, R2ILBlock, R2ILOp, SpaceId};
use r2sleigh_lift::{Disassembler, build_arch_spec, userop_map_for_arch};

use ruff_r2il::behavior::FunctionBehavior;
use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::facet::FacetPrefix;
use ruff_r2il::furnace::{self, Census, FactKind, FlatFact, HarvestReport};
use ruff_r2il::ore::OpTag;

// ================================================================================================
// Caps + FNV-1a 64 (labelled FNV — never a cryptographic hash; no hashing dependency).
// ================================================================================================

const DEFAULT_MAX_FUNCS: usize = 200;
const DEFAULT_MAX_SECTION_BYTES: usize = 262_144;

/// Minimum bytes libsla needs per lift call. Mirrors the private
/// `r2sleigh_lift::disasm::Disassembler::MIN_BYTES` (disasm.rs:301, see impl-spec §2) — every
/// window handed to `lift`/`lift_block` below is zero-padded up to this length.
const LIFT_MIN_BYTES: usize = 16;

const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 over raw bytes. Not a sha; labelled FNV throughout `PROVENANCE.md`.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS_64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash
}

fn env_cap(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

/// The pass-1 seven — the exact `OpTag` set both `minimal_pass_one()` and this harvest's
/// `R2ilConvention::from_arch` classify (impl-spec §5, §11's B2/prediction).
fn pass_one_seven() -> [OpTag; 7] {
    [
        OpTag::Copy,
        OpTag::IntAdd,
        OpTag::Load,
        OpTag::Store,
        OpTag::CBranch,
        OpTag::Call,
        OpTag::Return,
    ]
}

/// `ResidualReason` stable snake_case names (§8's `ResidualReason::ALL`/`as_str`) that can ONLY
/// fire on a row whose parent op is one of the pass-1 seven — see the derivation note this const
/// feeds into `TRIAGE-RESULT.md`'s B2 section. `ResidualFact` (§8) does not carry its parent's
/// `OpTag` directly, so B2's residual-side count is reconstructed from the furnace's own pass-1
/// ladder (§7): only the seven ever melt as an `Op` row in the first place, so every row whose
/// residual reason is NOT one of `opcode_not_in_convention` / `user_op_not_in_convention` /
/// `phi_fan_in_exceeds_predecessors` / `variadic_arity` / `no_facet_coordinate` — reasons that can
/// only fire on a non-seven opcode or a row with no parent op at all — must belong to a
/// seven-opcode parent. Labelled an APPROXIMATION in `TRIAGE-RESULT.md`, not an exact count.
/// Merged `ResidualLedger::grouped()` rows, keyed by `(shape_id.0, reason)` -> `(count, example
/// facet bytes)`. Aliased because the tuple-in-map shape trips `clippy::type_complexity` at both
/// its definition and its render-fn parameter.
type ReasonBucket = BTreeMap<(u64, &'static str), (usize, Option<[u8; 16]>)>;

/// Merged `ResidualLedger::by_address()` rows, keyed by `(resolved prefix, shape_id.0)` -> count.
type AddrBucket = BTreeMap<(Option<FacetPrefix>, u64), usize>;

const SEVEN_ELIGIBLE_RESIDUAL_REASONS: &[&str] = &[
    "no_convention_row_at_address",
    "indirect_target",
    "memory_object_escaped",
    "op_site_join_mismatch",
    "custom_space_not_in_convention",
    "facet_overflow_at_key",
];

// ================================================================================================
// mod elf — minimal, no-deps ELF64 LE reader. Every read bounds-checked; anything malformed
// returns `None` and the caller skips the whole binary with a printed note. Duplicated (not
// shared) against `r2il_corpus_profile.rs`'s own copy — W9 and W10 own disjoint files.
// ================================================================================================

mod elf {
    const EI_CLASS_64: u8 = 2;
    const EI_DATA_LE: u8 = 1;
    const EM_X86_64: u16 = 62;
    const SHT_SYMTAB: u32 = 2;
    const SHT_NOBITS: u32 = 8;
    const SHF_EXECINSTR: u64 = 0x4;
    const STT_FUNC: u8 = 2;

    pub(crate) struct Section {
        pub name: String,
        pub sh_type: u32,
        pub flags: u64,
        pub addr: u64,
        pub offset: u64,
        pub size: u64,
    }

    impl Section {
        pub(crate) fn is_exec(&self) -> bool {
            self.flags & SHF_EXECINSTR != 0 && self.sh_type != SHT_NOBITS
        }

        pub(crate) fn contains_range(&self, addr: u64, size: u64) -> bool {
            addr >= self.addr && addr.saturating_add(size) <= self.addr.saturating_add(self.size)
        }
    }

    pub(crate) struct FuncSym {
        pub name: String,
        pub value: u64,
        pub size: u64,
    }

    pub(crate) struct ElfInfo {
        pub sections: Vec<Section>,
        /// `STT_FUNC` symbols with `st_size > 0`, each already verified to sit inside a single
        /// executable section. Sorted, deduped by address.
        pub functions: Vec<FuncSym>,
        pub has_symtab: bool,
    }

    fn u16le(d: &[u8], off: usize) -> Option<u16> {
        let end = off.checked_add(2)?;
        Some(u16::from_le_bytes(d.get(off..end)?.try_into().ok()?))
    }
    fn u32le(d: &[u8], off: usize) -> Option<u32> {
        let end = off.checked_add(4)?;
        Some(u32::from_le_bytes(d.get(off..end)?.try_into().ok()?))
    }
    fn u64le(d: &[u8], off: usize) -> Option<u64> {
        let end = off.checked_add(8)?;
        Some(u64::from_le_bytes(d.get(off..end)?.try_into().ok()?))
    }

    fn cstr_at(strtab: &[u8], off: usize) -> String {
        if off >= strtab.len() {
            return String::new();
        }
        let end = strtab[off..]
            .iter()
            .position(|&b| b == 0)
            .map_or(strtab.len(), |p| off + p);
        String::from_utf8_lossy(&strtab[off..end]).into_owned()
    }

    struct RawShdr {
        name: u32,
        sh_type: u32,
        flags: u64,
        addr: u64,
        offset: u64,
        size: u64,
        link: u32,
        entsize: u64,
    }

    /// Parse an ELF64 LE x86-64 binary's header, section headers, and `STT_FUNC` symbol table.
    /// Returns `None` the instant any read is out of bounds or a fixed-field check fails.
    pub(crate) fn parse(data: &[u8]) -> Option<ElfInfo> {
        if data.len() < 64 {
            return None;
        }
        if &data[0..4] != b"\x7FELF" {
            return None;
        }
        if data[4] != EI_CLASS_64 || data[5] != EI_DATA_LE {
            return None;
        }
        if u16le(data, 18)? != EM_X86_64 {
            return None;
        }
        let e_shoff = u64le(data, 40)? as usize;
        let e_shentsize = u16le(data, 58)? as usize;
        let e_shnum = u16le(data, 60)? as usize;
        let e_shstrndx = u16le(data, 62)? as usize;
        if e_shentsize < 64 || e_shnum == 0 {
            return None;
        }

        let mut raw: Vec<RawShdr> = Vec::with_capacity(e_shnum);
        for i in 0..e_shnum {
            let rel = i.checked_mul(e_shentsize)?;
            let base = e_shoff.checked_add(rel)?;
            raw.push(RawShdr {
                name: u32le(data, base)?,
                sh_type: u32le(data, base + 4)?,
                flags: u64le(data, base + 8)?,
                addr: u64le(data, base + 16)?,
                offset: u64le(data, base + 24)?,
                size: u64le(data, base + 32)?,
                link: u32le(data, base + 40)?,
                entsize: u64le(data, base + 56)?,
            });
        }

        let shstrtab: &[u8] = if e_shstrndx < raw.len() {
            let s = &raw[e_shstrndx];
            let start = s.offset as usize;
            let end = start.checked_add(s.size as usize)?;
            data.get(start..end).unwrap_or(&[])
        } else {
            &[]
        };

        let sections: Vec<Section> = raw
            .iter()
            .map(|s| Section {
                name: cstr_at(shstrtab, s.name as usize),
                sh_type: s.sh_type,
                flags: s.flags,
                addr: s.addr,
                offset: s.offset,
                size: s.size,
            })
            .collect();

        let mut functions: Vec<FuncSym> = Vec::new();
        let mut has_symtab = false;
        if let Some(symtab) = raw.iter().find(|s| s.sh_type == SHT_SYMTAB) {
            has_symtab = true;
            if let Some(strtab_sec) = raw.get(symtab.link as usize) {
                let str_start = strtab_sec.offset as usize;
                let str_end = str_start
                    .checked_add(strtab_sec.size as usize)
                    .unwrap_or(str_start);
                let strtab = data.get(str_start..str_end).unwrap_or(&[]);
                let entsize = if symtab.entsize == 0 {
                    24
                } else {
                    symtab.entsize as usize
                };
                if entsize > 0 {
                    let count = (symtab.size as usize).checked_div(entsize).unwrap_or(0);
                    for i in 0..count {
                        let Some(rel) = i.checked_mul(entsize) else {
                            break;
                        };
                        let Some(base_u64) = symtab.offset.checked_add(rel as u64) else {
                            break;
                        };
                        let base = base_u64 as usize;
                        let Some(st_name) = u32le(data, base) else {
                            break;
                        };
                        let Some(&st_info) = data.get(base + 4) else {
                            break;
                        };
                        let Some(st_value) = u64le(data, base + 8) else {
                            break;
                        };
                        let Some(st_size) = u64le(data, base + 16) else {
                            break;
                        };
                        if st_info & 0xF != STT_FUNC || st_size == 0 {
                            continue;
                        }
                        let in_exec = sections
                            .iter()
                            .any(|sec| sec.is_exec() && sec.contains_range(st_value, st_size));
                        if in_exec {
                            functions.push(FuncSym {
                                name: cstr_at(strtab, st_name as usize),
                                value: st_value,
                                size: st_size,
                            });
                        }
                    }
                }
            }
        }
        functions.sort_by_key(|f| f.value);
        functions.dedup_by_key(|f| f.value);

        Some(ElfInfo {
            sections,
            functions,
            has_symtab,
        })
    }
}

// ================================================================================================
// Corpus resolution — mirrors r2il_corpus_profile.rs (§12): env override, then CLI args, then the
// fixed default four.
// ================================================================================================

fn corpus_paths() -> Vec<PathBuf> {
    if let Ok(v) = env::var("R2IL_CORPUS") {
        let paths: Vec<PathBuf> = v
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    let args: Vec<String> = env::args().skip(1).collect();
    if !args.is_empty() {
        return args.into_iter().map(PathBuf::from).collect();
    }
    let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../r2sleigh/tests/e2e/");
    vec![
        PathBuf::from(format!("{base}stress_test")),
        PathBuf::from(format!("{base}stress_test_opt")),
        PathBuf::from("/bin/ls"),
        PathBuf::from("/usr/bin/env"),
    ]
}

// ================================================================================================
// Leader / basic-block detection + lift — the §12 "function boundaries from symtab, leaders
// approximate" method, reused here to feed `FunctionBehavior::from_blocks_raw`.
// ================================================================================================

fn pad_window(bytes: &[u8], want: usize) -> Vec<u8> {
    let want = want.max(LIFT_MIN_BYTES);
    let mut w: Vec<u8> = bytes.iter().take(want).copied().collect();
    if w.len() < want {
        w.resize(want, 0);
    }
    w
}

/// leaders = `{func_addr}` ∪ intra-function const branch/call targets ∪ `{addr after any
/// control-flow op}`. A single-instruction linear sweep via `disasm.lift`, exactly like §12's
/// Pass 1 op-level sweep, just also recording leaders as it goes.
fn find_leaders(disasm: &Disassembler, code: &[u8], func_addr: u64) -> BTreeSet<u64> {
    let func_end = func_addr + code.len() as u64;
    let mut leaders: BTreeSet<u64> = BTreeSet::new();
    leaders.insert(func_addr);

    let mut offset = 0usize;
    while offset < code.len() {
        let addr = func_addr + offset as u64;
        let window = pad_window(&code[offset..], LIFT_MIN_BYTES);
        match disasm.lift(&window, addr) {
            Ok(block) => {
                let size = (block.size as usize).max(1);
                let mut had_cf = false;
                for op in &block.ops {
                    if op.is_control_flow() {
                        had_cf = true;
                    }
                    let target = match op {
                        R2ILOp::Branch { target } => Some(target),
                        R2ILOp::CBranch { target, .. } => Some(target),
                        R2ILOp::Call { target } => Some(target),
                        _ => None,
                    };
                    if let Some(t) = target
                        && t.space == SpaceId::Const
                        && t.offset >= func_addr
                        && t.offset < func_end
                    {
                        leaders.insert(t.offset);
                    }
                }
                if had_cf {
                    let after = addr + size as u64;
                    if after < func_end {
                        leaders.insert(after);
                    }
                }
                offset += size;
            }
            Err(_) => {
                offset += 1;
            }
        }
    }
    leaders
}

/// Basic blocks = maximal ranges between leaders, the last one running to the function's end.
/// One `disasm.lift_block` per range — `R2ILBlock`s ready for `FunctionBehavior::from_blocks_raw`.
fn lift_function_blocks(disasm: &Disassembler, code: &[u8], func_addr: u64) -> Vec<R2ILBlock> {
    if code.is_empty() {
        return Vec::new();
    }
    let func_end = func_addr + code.len() as u64;
    let leaders = find_leaders(disasm, code, func_addr);
    let mut boundaries: Vec<u64> = leaders.into_iter().collect();
    if boundaries.last() != Some(&func_end) {
        boundaries.push(func_end);
    }
    boundaries.dedup();

    let mut blocks = Vec::new();
    for w in boundaries.windows(2) {
        let bb_addr = w[0];
        let bb_end = w[1];
        if bb_end <= bb_addr {
            continue;
        }
        let bb_len = (bb_end - bb_addr) as usize;
        let start_off = (bb_addr - func_addr) as usize;
        if start_off >= code.len() {
            continue;
        }
        let end_off = (start_off + bb_len).min(code.len());
        let raw = &code[start_off..end_off];
        let window = pad_window(raw, bb_len);
        if let Ok(block) = disasm.lift_block(&window, bb_addr, bb_len)
            && !block.ops.is_empty()
        {
            blocks.push(block);
        }
    }
    blocks
}

// ================================================================================================
// Accumulators
// ================================================================================================

struct OreRow {
    binary: String,
    func_hex: String,
    fact: FlatFact,
}

struct CorpusEntry {
    path: String,
    len: usize,
    fnv: Option<u64>,
    status: String,
}

struct Accum {
    rows: Vec<OreRow>,
    /// Merged `ResidualLedger::grouped()` output across every harvested function, keyed by
    /// `(shape_id.0, reason)`.
    reason_bucket: ReasonBucket,
    /// Merged `ResidualLedger::by_address()` output across every harvested function.
    addr_bucket: AddrBucket,
    combined: HarvestReport,
    remaining_func_budget: usize,
}

// ================================================================================================
// Per-binary / per-function driver
// ================================================================================================

/// The three handles that stay INVARIANT across every function of every binary: one
/// `Disassembler`, one `ArchSpec`, one `R2ilConvention` (built once by `from_arch`, per §11).
/// Bundled so the per-binary driver takes a context rather than eight positional parameters —
/// the grouping is the readable shape, and it also settles `clippy::too_many_arguments`.
struct LiftCtx<'a> {
    disasm: &'a Disassembler,
    spec: &'a ArchSpec,
    conv: &'a R2ilConvention,
}

fn process_elf_symtab_functions(
    ctx: &LiftCtx<'_>,
    binary_label: &str,
    data: &[u8],
    info: &elf::ElfInfo,
    max_section_bytes: usize,
    accum: &mut Accum,
) -> usize {
    let mut processed = 0usize;
    for func in &info.functions {
        if accum.remaining_func_budget == 0 {
            eprintln!(
                "[harvest] {binary_label}: function budget (R2IL_HARVEST_MAX_FUNCS) exhausted, stopping this binary"
            );
            break;
        }

        let Some(sec) = info
            .sections
            .iter()
            .find(|s| s.is_exec() && s.contains_range(func.value, func.size))
        else {
            eprintln!(
                "[harvest] {binary_label}: fn @ {:#x} not inside a single exec section, skip",
                func.value
            );
            continue;
        };

        let sec_size = (sec.size as usize).min(max_section_bytes);
        let sec_start = sec.offset as usize;
        let Some(sec_end) = sec_start.checked_add(sec_size) else {
            continue;
        };
        let Some(sec_bytes) = data.get(sec_start..sec_end) else {
            eprintln!(
                "[harvest] {binary_label}: section '{}' out of file bounds, skip",
                sec.name
            );
            continue;
        };

        if func.value < sec.addr {
            continue;
        }
        let func_off_in_sec = (func.value - sec.addr) as usize;
        if func_off_in_sec >= sec_bytes.len() {
            eprintln!(
                "[harvest] {binary_label}: fn '{}' @ {:#x} falls outside the {max_section_bytes} byte section cap, skip",
                func.name, func.value
            );
            continue;
        }
        let func_end_in_sec = func_off_in_sec
            .saturating_add(func.size as usize)
            .min(sec_bytes.len());
        let code = &sec_bytes[func_off_in_sec..func_end_in_sec];
        if code.is_empty() {
            continue;
        }

        let blocks = lift_function_blocks(ctx.disasm, code, func.value);
        if blocks.is_empty() {
            eprintln!(
                "[harvest] {binary_label}: fn '{}' @ {:#x} produced no liftable blocks, skip",
                func.name, func.value
            );
            continue;
        }

        let Some(behavior) = FunctionBehavior::from_blocks_raw(&blocks, Some(ctx.spec)) else {
            eprintln!(
                "[harvest] {binary_label}: fn '{}' @ {:#x} — from_blocks_raw returned None, skip",
                func.name, func.value
            );
            continue;
        };
        let behavior = behavior.with_name(func.name.clone());

        let (facts, ledger, report) = furnace::smelt(&behavior, &blocks, ctx.conv);

        accum.combined.harvested += report.harvested;
        accum.combined.classified += report.classified;
        accum.combined.residual += report.residual;
        accum.combined.dropped += report.dropped;

        let func_hex = format!("{:#x}", func.value);
        for fact in &facts {
            accum.rows.push(OreRow {
                binary: binary_label.to_string(),
                func_hex: func_hex.clone(),
                fact: *fact,
            });
        }

        for (shape_id, reason, count, example) in ledger.grouped() {
            let entry = accum
                .reason_bucket
                .entry((shape_id.0, reason))
                .or_insert((0usize, None));
            entry.0 += count;
            if entry.1.is_none() {
                entry.1 = example.map(|f| f.0);
            }
        }
        for (prefix, shape_id, count) in ledger.by_address() {
            *accum.addr_bucket.entry((prefix, shape_id.0)).or_insert(0) += count;
        }

        accum.remaining_func_budget -= 1;
        processed += 1;
    }
    processed
}

// ================================================================================================
// Rendering — TSV / Markdown / TOML. Every header states "evidence, never a re-ingest path".
// ================================================================================================

fn hex16(bytes: &[u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn opt_dbg<T: std::fmt::Debug>(v: &Option<T>) -> String {
    match v {
        Some(x) => format!("{x:?}"),
        None => "-".to_string(),
    }
}

fn facet_prefix_str(p: &Option<FacetPrefix>) -> String {
    match p {
        None => "-".to_string(),
        Some(FacetPrefix::Space { discriminant }) => format!("space:{discriminant}"),
        Some(FacetPrefix::SpaceOffset {
            discriminant,
            offset,
        }) => format!("space:{discriminant}/offset:{offset:#x}"),
        Some(FacetPrefix::SpaceOffsetSize {
            discriminant,
            offset,
            size,
        }) => format!("space:{discriminant}/offset:{offset:#x}/size:{size}"),
    }
}

fn render_ore_tsv(rows: &[OreRow]) -> String {
    let mut out = String::new();
    out.push_str(
        "# R2IL pass-1 ore — evidence, never a re-ingest path. Nothing in ruff parses this back.\n",
    );
    out.push_str(
        "#schema binary\tfunction\tfact_id\tat\tconcern\tkind\topcode\ta\tb\tprov_inst\tprov_block\tprov_op_site\tprov_value\n",
    );
    out.push_str("#version 1\n");
    for row in rows {
        let f = &row.fact;
        let op_site = match f.prov.op_site {
            Some((addr, idx)) => format!("{addr:#x}:{idx}"),
            None => "-".to_string(),
        };
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{:?}\t{:?}\t{}\t{}\t{}\t{}\t{}\t{op_site}\t{}\n",
            row.binary,
            row.func_hex,
            f.id.0,
            hex16(&f.at.0),
            f.concern,
            f.kind,
            f.opcode.as_str(),
            f.a,
            f.b,
            opt_dbg(&f.prov.inst),
            opt_dbg(&f.prov.block),
            opt_dbg(&f.prov.value),
        ));
    }
    out
}

fn render_census_md(all_facts: &[FlatFact]) -> String {
    let census: Census = furnace::census(all_facts);
    let mut out = String::new();
    out.push_str("# R2IL pass-1 census\n\n");
    out.push_str(&format!(
        "Total classified `FlatFact` rows: {}\n\n",
        all_facts.len()
    ));
    out.push_str("## By fact kind\n\n| kind | count |\n|---|---|\n");
    for (k, v) in &census.by_fact_kind {
        out.push_str(&format!("| {k} | {v} |\n"));
    }
    out.push_str("\n## By opcode\n\n| opcode | count |\n|---|---|\n");
    for (k, v) in &census.by_opcode {
        out.push_str(&format!("| {k} | {v} |\n"));
    }
    out
}

fn render_slag_tsv(reason_bucket: &ReasonBucket, addr_bucket: &AddrBucket) -> String {
    let mut grouped: Vec<(u64, &'static str, usize, Option<[u8; 16]>)> = Vec::new();
    for (&(shape_id, reason), &(count, example)) in reason_bucket.iter() {
        grouped.push((shape_id, reason, count, example));
    }
    grouped.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));

    let mut by_addr: Vec<(Option<FacetPrefix>, u64, usize)> = Vec::new();
    for (&(prefix, shape_id), &count) in addr_bucket.iter() {
        by_addr.push((prefix, shape_id, count));
    }
    by_addr.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.1.cmp(&b.1)));

    let mut out = String::new();
    out.push_str(
        "# R2IL pass-1 slag — the addressed residual ledger. Evidence, never a re-ingest path.\n",
    );
    out.push_str("#version 1\n");
    out.push_str(
        "#section grouped — ResidualLedger::grouped(), merged across every harvested function\n",
    );
    out.push_str("#schema section\tshape_id\treason\tcount\texample_facet\n");
    for (shape_id, reason, count, example) in &grouped {
        let facet = match example {
            Some(bytes) => hex16(bytes),
            None => "-".to_string(),
        };
        out.push_str(&format!(
            "grouped\t{shape_id:016x}\t{reason}\t{count}\t{facet}\n"
        ));
    }
    out.push_str(
        "#section by_address — ResidualLedger::by_address(), the proposer's work queue, merged across every harvested function\n",
    );
    out.push_str("#schema section\tprefix\tshape_id\tcount\n");
    for (prefix, shape_id, count) in &by_addr {
        out.push_str(&format!(
            "by_address\t{}\t{shape_id:016x}\t{count}\n",
            facet_prefix_str(prefix)
        ));
    }
    out
}

fn render_provenance_md(
    entries: &[CorpusEntry],
    arch: &str,
    max_funcs: usize,
    max_section_bytes: usize,
) -> String {
    let mut out = String::new();
    out.push_str("# R2IL pass-1 harvest — PROVENANCE\n\n");
    out.push_str(
        "Hashes below are **FNV-1a 64** (not a cryptographic hash) over the raw file bytes, computed inline in this example — no hashing dependency.\n\n",
    );
    out.push_str("## Corpus\n\n| path | bytes | fnv1a64 | status |\n|---|---|---|---|\n");
    for e in entries {
        let fnv = e
            .fnv
            .map_or_else(|| "-".to_string(), |h| format!("{h:016x}"));
        out.push_str(&format!(
            "| {} | {} | {} | {} |\n",
            e.path, e.len, fnv, e.status
        ));
    }
    out.push_str("\n## Environment\n\n");
    out.push_str("- `r2sleigh` commit: `60942f6`\n");
    out.push_str(&format!("- Architecture: `{arch}`\n"));
    out.push_str(
        "- `sleigh-config` = \"1.0\", feature `x86` (exact resolved patch pinned by the committed `Cargo.lock`)\n",
    );
    out.push_str(
        "- Convention: `R2ilConvention::from_arch(&spec, [Copy, IntAdd, Load, Store, CBranch, Call, Return])` — one convention, built once, reused for every harvested function\n",
    );
    out.push_str(&format!(
        "- Caps in force: `R2IL_HARVEST_MAX_FUNCS={max_funcs}`, `R2IL_HARVEST_MAX_SECTION_BYTES={max_section_bytes}`\n",
    ));
    out.push_str("\n## Invocation\n\n```sh\ncargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example harvest_r2il\n```\n");
    out
}

#[allow(clippy::too_many_arguments)]
fn render_triage_md(
    max_funcs: usize,
    max_section_bytes: usize,
    combined: &HarvestReport,
    functions_processed: usize,
    classified_seven: usize,
    residual_seven_eligible: usize,
    dominant_share: f64,
    distinct_shapes: usize,
    residual_addressed_note: &str,
    op_classified: usize,
    op_total: usize,
) -> String {
    let mut out = String::new();
    out.push_str("# R2IL pass-1 TRIAGE RESULT\n\n");
    out.push_str(&format!(
        "Caps in force: `R2IL_HARVEST_MAX_FUNCS={max_funcs}`, `R2IL_HARVEST_MAX_SECTION_BYTES={max_section_bytes}`\n\n",
    ));

    out.push_str("## Pre-registered bars (stated BEFORE the measured section below)\n\n");
    out.push_str(
        "- **B1 — conservation (absolute).** `dropped == 0` and `harvested == classified + residual`. Any violation **KILLS** the pass: the enumerator, not the corpus, is wrong. Not a percentage.\n",
    );
    out.push_str(
        "- **B2 — coverage of the declared seven.** Of ore facts whose parent opcode is one of `{Copy, IntAdd, Load, Store, CBranch, Call, Return}`, **>=99% classify -> PASS; <90% -> KILL.** The 90-99% band is INVESTIGATE (expected causes: operand rows with no convention row at their address, `CallSite` rows with no `direct_target` — both legitimate slag under a parent that classified).\n",
    );
    out.push_str(
        "- **B3 — the slag is named and addressed, not lumped.** `residual > 0`, distinct `shape_id` count **>= 5**, `dominant_share() < 0.60`, and **every** residual except `NoFacetCoordinate` carries `at.is_some()`. `residual == 0` is a **KILL** too — it means someone widened the ladder.\n\n",
    );
    out.push_str(
        "Also **pre-register a prediction that is NOT a bar** (so it can be wrong without moving a goalpost): on an x86-64 corpus `Copy/IntAdd/Load/Store` dominate, so pass 1 is expected to classify roughly **60-80%** of all `Op` facts. Record the measured figure either way.\n\n",
    );
    out.push_str("---\n\n## Measured\n\n");

    out.push_str(&format!("Functions harvested: {functions_processed}\n\n"));
    out.push_str(&format!(
        "Conservation line: harvested {} / classified {} / residual {} / dropped {}\n\n",
        combined.harvested, combined.classified, combined.residual, combined.dropped
    ));

    let b1_dropped = combined.dropped == 0;
    let b1_conserved = combined.harvested == combined.classified + combined.residual;
    let b1 = b1_dropped && b1_conserved;
    out.push_str(&format!(
        "**B1: {}** — dropped == 0: {b1_dropped}; harvested == classified + residual: {b1_conserved}\n\n",
        if b1 { "PASS" } else { "KILL" }
    ));

    let b2_denominator = classified_seven + residual_seven_eligible;
    let b2_pct = if b2_denominator == 0 {
        100.0
    } else {
        classified_seven as f64 / b2_denominator as f64 * 100.0
    };
    let b2_verdict = if b2_pct >= 99.0 {
        "PASS"
    } else if b2_pct < 90.0 {
        "KILL"
    } else {
        "INVESTIGATE"
    };
    out.push_str(&format!(
        "**B2: {b2_verdict}** — {classified_seven} classified / {b2_denominator} total ore facts under a seven-opcode parent = {b2_pct:.2}%.\n\n",
    ));
    out.push_str(
        "  Derivation note: `ResidualFact` does not carry its parent opcode directly, so the denominator's residual half is APPROXIMATED by summing residual reasons that can *only* fire on a row whose parent op is one of the seven (`no_convention_row_at_address`, `indirect_target`, `memory_object_escaped`, `op_site_join_mismatch`, `custom_space_not_in_convention`, `facet_overflow_at_key`) — reasons that can only fire on a non-seven or no-parent-op row (`opcode_not_in_convention`, `user_op_not_in_convention`, `phi_fan_in_exceeds_predecessors`, `variadic_arity`, `no_facet_coordinate`) are excluded. Labelled APPROXIMATION, not exact — see the module doc comment above `SEVEN_ELIGIBLE_RESIDUAL_REASONS`.\n\n",
    );

    let b3_residual = combined.residual > 0;
    let b3_shapes = distinct_shapes >= 5;
    let b3_share = dominant_share < 0.60;
    let b3 = b3_residual && b3_shapes && b3_share;
    out.push_str(&format!(
        "**B3: {}** — residual > 0: {b3_residual} ({}); distinct shape_id count: {distinct_shapes} (>=5: {b3_shapes}); dominant_share: {dominant_share:.3} (<0.60: {b3_share}).\n\n",
        if b3 { "PASS" } else { "KILL" },
        combined.residual,
    ));
    out.push_str(&format!("  {residual_addressed_note}\n\n"));

    let op_pct = if op_total == 0 {
        0.0
    } else {
        op_classified as f64 / op_total as f64 * 100.0
    };
    let within = (60.0..=80.0).contains(&op_pct);
    out.push_str(&format!(
        "**Non-bar prediction, measured:** {op_classified} / {op_total} `Op` facts classified = {op_pct:.2}% (predicted 60-80%; {}).\n",
        if within {
            "within the predicted band"
        } else {
            "OUTSIDE the predicted band — recorded honestly, not a bar"
        }
    ));

    out
}

// ================================================================================================
// main
// ================================================================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let max_funcs = env_cap("R2IL_HARVEST_MAX_FUNCS", DEFAULT_MAX_FUNCS);
    let max_section_bytes = env_cap("R2IL_HARVEST_MAX_SECTION_BYTES", DEFAULT_MAX_SECTION_BYTES);

    let out_dir = env::var("R2IL_HARVEST_OUT").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../.claude/harvest/r2il").to_string()
    });
    fs::create_dir_all(&out_dir)?;
    eprintln!("[harvest] output directory: {out_dir}");
    eprintln!(
        "[harvest] caps: R2IL_HARVEST_MAX_FUNCS={max_funcs} R2IL_HARVEST_MAX_SECTION_BYTES={max_section_bytes}"
    );

    let spec = build_arch_spec(
        sleigh_config::processor_x86::SLA_X86_64,
        sleigh_config::processor_x86::PSPEC_X86_64,
        "x86-64",
    )?;
    let mut disasm = Disassembler::from_sla(
        sleigh_config::processor_x86::SLA_X86_64,
        sleigh_config::processor_x86::PSPEC_X86_64,
        "x86-64",
    )?;
    disasm.set_userop_map(userop_map_for_arch("x86-64"));

    let conv = R2ilConvention::from_arch(&spec, pass_one_seven())
        .map_err(|e| format!("R2ilConvention::from_arch(x86-64) failed: {e}"))?;
    eprintln!(
        "[harvest] convention built from {} registers, {} userops",
        spec.registers.len(),
        spec.userops.len()
    );

    let mut entries: Vec<CorpusEntry> = Vec::new();
    let mut accum = Accum {
        rows: Vec::new(),
        reason_bucket: BTreeMap::new(),
        addr_bucket: BTreeMap::new(),
        combined: HarvestReport::default(),
        remaining_func_budget: max_funcs,
    };
    let mut functions_processed = 0usize;

    for path in corpus_paths() {
        let label = path.display().to_string();
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[harvest] {label}: skip — read failed: {e}");
                entries.push(CorpusEntry {
                    path: label,
                    len: 0,
                    fnv: None,
                    status: format!("skipped (read failed: {e})"),
                });
                continue;
            }
        };
        let fnv = fnv1a64(&data);
        eprintln!(
            "[harvest] {label}: {} bytes, fnv1a64={fnv:016x}",
            data.len()
        );

        let Some(info) = elf::parse(&data) else {
            eprintln!("[harvest] {label}: skip — not a recognized ELF64 LE x86-64 binary");
            entries.push(CorpusEntry {
                path: label,
                len: data.len(),
                fnv: Some(fnv),
                status: "skipped (not ELF64 LE x86-64)".to_string(),
            });
            continue;
        };

        if !info.has_symtab {
            eprintln!(
                "[harvest] {label}: skip — stripped (no symtab); function-level harvest needs symtab boundaries"
            );
            entries.push(CorpusEntry {
                path: label,
                len: data.len(),
                fnv: Some(fnv),
                status: "skipped (no symtab)".to_string(),
            });
            continue;
        }

        if accum.remaining_func_budget == 0 {
            eprintln!(
                "[harvest] {label}: skip — function budget (R2IL_HARVEST_MAX_FUNCS={max_funcs}) already exhausted"
            );
            entries.push(CorpusEntry {
                path: label,
                len: data.len(),
                fnv: Some(fnv),
                status: "skipped (function budget exhausted)".to_string(),
            });
            continue;
        }

        eprintln!(
            "[harvest] {label}: {} STT_FUNC symbols in executable sections",
            info.functions.len()
        );

        let processed = process_elf_symtab_functions(
            &LiftCtx {
                disasm: &disasm,
                spec: &spec,
                conv: &conv,
            },
            &label,
            &data,
            &info,
            max_section_bytes,
            &mut accum,
        );
        functions_processed += processed;
        entries.push(CorpusEntry {
            path: label,
            len: data.len(),
            fnv: Some(fnv),
            status: format!("harvested ({processed} functions)"),
        });
    }

    eprintln!(
        "[harvest] done: {functions_processed} functions across {} binaries, {} FlatFact rows, conservation {} / {} / {} / {}",
        entries.len(),
        accum.rows.len(),
        accum.combined.harvested,
        accum.combined.classified,
        accum.combined.residual,
        accum.combined.dropped
    );

    let facts_only: Vec<FlatFact> = accum.rows.iter().map(|r| r.fact).collect();

    // ---- artifact 1: ore.tsv ----
    let ore_path = format!("{out_dir}/r2il-pass1.ore.tsv");
    fs::write(&ore_path, render_ore_tsv(&accum.rows))?;
    eprintln!("[harvest] wrote {ore_path} ({} rows)", accum.rows.len());

    // ---- artifact 2: census.md ----
    let census_path = format!("{out_dir}/r2il-pass1-census.md");
    fs::write(&census_path, render_census_md(&facts_only))?;
    eprintln!("[harvest] wrote {census_path}");

    // ---- artifact 3: slag.tsv ----
    let slag_path = format!("{out_dir}/r2il-pass1-slag.tsv");
    fs::write(
        &slag_path,
        render_slag_tsv(&accum.reason_bucket, &accum.addr_bucket),
    )?;
    eprintln!("[harvest] wrote {slag_path}");

    // ---- artifact 4: convention.toml ----
    let conv_path = format!("{out_dir}/r2il-convention.toml");
    fs::write(&conv_path, conv.to_toml())?;
    eprintln!("[harvest] wrote {conv_path}");

    // ---- artifact 5: PROVENANCE.md ----
    let prov_path = format!("{out_dir}/PROVENANCE.md");
    fs::write(
        &prov_path,
        render_provenance_md(&entries, &spec.name, max_funcs, max_section_bytes),
    )?;
    eprintln!("[harvest] wrote {prov_path}");

    // ---- artifact 6: TRIAGE-RESULT.md ----
    let seven = pass_one_seven();
    let mut classified_seven = 0usize;
    let mut op_classified = 0usize;
    for f in &facts_only {
        if seven.contains(&f.opcode) {
            classified_seven += 1;
        }
        if matches!(f.kind, FactKind::Op) {
            op_classified += 1;
        }
    }

    let mut distinct_shape_ids: BTreeSet<u64> = BTreeSet::new();
    let mut total_residual_grouped = 0usize;
    let mut max_group = 0usize;
    let mut residual_seven_eligible = 0usize;
    let mut op_residual = 0usize;
    let mut missing_facet_reasons: Vec<String> = Vec::new();
    for (&(shape_id, reason), &(count, example)) in accum.reason_bucket.iter() {
        distinct_shape_ids.insert(shape_id);
        total_residual_grouped += count;
        if count > max_group {
            max_group = count;
        }
        if SEVEN_ELIGIBLE_RESIDUAL_REASONS.contains(&reason) {
            residual_seven_eligible += count;
        }
        if reason == "opcode_not_in_convention" {
            op_residual += count;
        }
        if reason != "no_facet_coordinate" && example.is_none() {
            missing_facet_reasons.push(format!("shape {shape_id:016x} ({reason})"));
        }
    }
    let dominant_share = if total_residual_grouped == 0 {
        0.0
    } else {
        max_group as f64 / total_residual_grouped as f64
    };
    let distinct_shapes = distinct_shape_ids.len();
    let op_total = op_classified + op_residual;

    let residual_addressed_note = if missing_facet_reasons.is_empty() {
        "Spot check: every grouped bucket except no_facet_coordinate reports an example facet address.".to_string()
    } else {
        format!(
            "Spot check FAILED for: {}. (grouped() reports one example per shape — see slag.rs's own row-level tests for the per-row invariant this does not, by itself, disprove.)",
            missing_facet_reasons.join(", ")
        )
    };

    let triage_path = format!("{out_dir}/TRIAGE-RESULT.md");
    fs::write(
        &triage_path,
        render_triage_md(
            max_funcs,
            max_section_bytes,
            &accum.combined,
            functions_processed,
            classified_seven,
            residual_seven_eligible,
            dominant_share,
            distinct_shapes,
            &residual_addressed_note,
            op_classified,
            op_total,
        ),
    )?;
    eprintln!("[harvest] wrote {triage_path}");

    Ok(())
}
