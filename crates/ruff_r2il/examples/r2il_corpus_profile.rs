//! §12 corpus profile — plain text to stdout, no serde, no file writes.
//!
//! Two independent passes over a small, fixed x86-64 ELF64 corpus:
//!
//! - **Pass 1 — `== MEASURED EXACT (op level) ==`.** A linear byte sweep of every executable
//!   section, one [`r2sleigh_lift::Disassembler::lift`] call per native instruction. This is
//!   EXACT at the op level: every metric here is read straight off the `R2ILOp`s libsla actually
//!   produced, never inferred.
//! - **Pass 2 — `== HEURISTIC-DERIVED (function / CFG level) ==`.** Symtab-bearing binaries
//!   only. `STT_FUNC` boundaries come straight from the symbol table and are EXACT; the
//!   basic-block leader set inside each function (intra-function constant branch targets, plus
//!   the address after every control-flow op) is an APPROXIMATION — indirect branches and jump
//!   tables are not resolved. **Non-negotiable labelling rule:** every row this pass prints is
//!   under the `HEURISTIC-DERIVED` heading, and the heading is paired with the exact caveat
//!   sentence below, every time. A stripped binary (no symtab) never contributes a heuristic row
//!   at all — it prints the explicit skip line instead of a silent omission.
//!
//! Nothing here is copied into ruff: the corpus lives outside the repository (siblings under
//! `r2sleigh/tests/e2e/`, or system binaries), and this example only ever reads it.
//!
//! No `unwrap`/`panic` on corpus input: every ELF field read is bounds-checked and returns
//! `Option`; a malformed or unreadable binary is skipped with a printed note, never a crash.
//! Every metric in both passes is read from `R2ILOp`'s own accessors (`inputs()`, `output()`,
//! `is_control_flow()`, `is_memory_read()`, `is_memory_write()`, and a typed `match` on the op's
//! own variant for the atomic check) — never from `Display`/`Debug` formatting of an op.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use r2il::{ArchSpec, R2ILBlock, R2ILOp, SpaceId};
use r2sleigh_lift::{Disassembler, build_arch_spec, userop_map_for_arch};

use ruff_r2il::behavior::FunctionBehavior;
use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::facet;
use ruff_r2il::furnace;
use ruff_r2il::ore::OpTag;
use ruff_r2il::vocab::VocabHarvest;

/// Minimum bytes libsla needs per lift call. Mirrors the private
/// `r2sleigh_lift::disasm::Disassembler::MIN_BYTES` — every window handed to `lift`/`lift_block`
/// is zero-padded up to this length.
const MIN_LIFT_BYTES: usize = 16;

const DEFAULT_MAX_FUNCS: usize = 200;
const DEFAULT_MAX_SECTION_BYTES: usize = 262_144;

const ELF_EXEC_FLAG: u64 = 0x4;
const ELF_SHT_NOBITS: u32 = 8;
const ELF_SHT_SYMTAB: u32 = 2;
const ELF_STT_FUNC: u8 = 2;

// ============================================================================================
// mod elf — minimal, no-deps ELF64 LE reader. Every read bounds-checked; anything malformed
// returns `None` and the caller skips the whole binary with a printed note. This never parses
// anything beyond the header / section headers / symtab needed for the two passes above.
// ============================================================================================
mod elf {
    use super::{ELF_EXEC_FLAG, ELF_SHT_NOBITS, ELF_SHT_SYMTAB, ELF_STT_FUNC};

    pub(crate) struct Section {
        pub(crate) name: String,
        pub(crate) sh_type: u32,
        pub(crate) flags: u64,
        pub(crate) addr: u64,
        pub(crate) offset: u64,
        pub(crate) size: u64,
    }

    impl Section {
        pub(crate) fn is_exec(&self) -> bool {
            (self.flags & ELF_EXEC_FLAG) != 0 && self.sh_type != ELF_SHT_NOBITS
        }

        pub(crate) fn end_addr(&self) -> u64 {
            self.addr.saturating_add(self.size)
        }
    }

    pub(crate) struct Symbol {
        pub(crate) name: String,
        pub(crate) value: u64,
        pub(crate) size: u64,
    }

    pub(crate) struct Info {
        pub(crate) sections: Vec<Section>,
        /// `STT_FUNC` symbols with `st_size > 0`, each verified to sit inside an executable
        /// section at parse time.
        pub(crate) functions: Vec<Symbol>,
    }

    fn read_u16(buf: &[u8], off: usize) -> Option<u16> {
        let b = buf.get(off..off + 2)?;
        Some(u16::from_le_bytes([b[0], b[1]]))
    }

    fn read_u32(buf: &[u8], off: usize) -> Option<u32> {
        let b = buf.get(off..off + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn read_u64(buf: &[u8], off: usize) -> Option<u64> {
        let b = buf.get(off..off + 8)?;
        Some(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn read_u8(buf: &[u8], off: usize) -> Option<u8> {
        buf.get(off).copied()
    }

    /// NUL-terminated name from a string table section, at `strtab_off + name_off`.
    fn read_name(bytes: &[u8], strtab_off: u64, strtab_size: u64, name_off: u32) -> Option<String> {
        let start = strtab_off.checked_add(u64::from(name_off))?;
        let limit = strtab_off.checked_add(strtab_size)?;
        if start >= limit {
            return None;
        }
        let start = usize::try_from(start).ok()?;
        let limit = usize::try_from(limit).ok()?.min(bytes.len());
        let slice = bytes.get(start..limit)?;
        let end = slice.iter().position(|&b| b == 0).unwrap_or(slice.len());
        std::str::from_utf8(&slice[..end]).ok().map(str::to_string)
    }

    /// Parse an ELF64 LE x86-64 binary's header, section headers, and `STT_FUNC` symbol table.
    /// Every offset below is the field's byte offset per the ELF64 spec, as documented in the
    /// impl-spec. Returns `None` the moment any read is out of bounds or a fixed-field check
    /// fails — the caller treats that as "skip this binary, print a note", never a panic.
    pub(crate) fn parse(bytes: &[u8]) -> Option<Info> {
        let magic = bytes.get(0..4)?;
        if magic != [0x7f, b'E', b'L', b'F'] {
            return None;
        }
        if read_u8(bytes, 4)? != 2 {
            // EI_CLASS: ELFCLASS64
            return None;
        }
        if read_u8(bytes, 5)? != 1 {
            // EI_DATA: ELFDATA2LSB
            return None;
        }
        if read_u16(bytes, 18)? != 62 {
            // e_machine: EM_X86_64
            return None;
        }
        let e_shoff = read_u64(bytes, 40)?;
        let e_shentsize = read_u16(bytes, 58)?;
        let e_shnum = read_u16(bytes, 60)?;
        let e_shstrndx = read_u16(bytes, 62)?;
        if e_shentsize < 64 || e_shnum == 0 {
            return None;
        }

        let section_header_off = |index: u16| -> Option<usize> {
            let off = e_shoff.checked_add(
                u64::from(index).checked_mul(u64::from(e_shentsize))?,
            )?;
            usize::try_from(off).ok()
        };

        let shstr_hdr = section_header_off(e_shstrndx)?;
        let shstrtab_offset = read_u64(bytes, shstr_hdr + 24)?;
        let shstrtab_size = read_u64(bytes, shstr_hdr + 32)?;

        let mut sections = Vec::with_capacity(usize::from(e_shnum));
        for index in 0..e_shnum {
            let hdr = section_header_off(index)?;
            let sh_name = read_u32(bytes, hdr)?;
            let sh_type = read_u32(bytes, hdr + 4)?;
            let sh_flags = read_u64(bytes, hdr + 8)?;
            let sh_addr = read_u64(bytes, hdr + 16)?;
            let sh_offset = read_u64(bytes, hdr + 24)?;
            let sh_size = read_u64(bytes, hdr + 32)?;
            let sh_link = read_u32(bytes, hdr + 40)?;
            let sh_entsize = read_u64(bytes, hdr + 56)?;
            let name = read_name(bytes, shstrtab_offset, shstrtab_size, sh_name)
                .unwrap_or_default();
            sections.push((
                Section {
                    name,
                    sh_type,
                    flags: sh_flags,
                    addr: sh_addr,
                    offset: sh_offset,
                    size: sh_size,
                },
                sh_link,
                sh_entsize,
            ));
        }

        let mut functions = Vec::new();
        if let Some((symtab, link, entsize)) =
            sections.iter().find(|(s, _, _)| s.sh_type == ELF_SHT_SYMTAB)
        {
            let entsize = if *entsize == 0 { 24 } else { *entsize };
            if let Some((strtab, _, _)) = sections.get(*link as usize) {
                let count = symtab.size / entsize;
                for i in 0..count {
                    let off = symtab.offset.checked_add(i.checked_mul(entsize)?)?;
                    let off = usize::try_from(off).ok()?;
                    let st_name = read_u32(bytes, off)?;
                    let st_info = read_u8(bytes, off + 4)?;
                    let st_value = read_u64(bytes, off + 8)?;
                    let st_size = read_u64(bytes, off + 16)?;
                    let is_func = (st_info & 0xF) == ELF_STT_FUNC;
                    if !is_func || st_size == 0 {
                        continue;
                    }
                    let in_exec = sections.iter().any(|(s, _, _)| {
                        s.is_exec() && st_value >= s.addr && st_value < s.end_addr()
                    });
                    if !in_exec {
                        continue;
                    }
                    // A malformed/unreadable name still yields a usable symbol (name just
                    // falls back to empty) — only value/size gate whether the symbol is used.
                    let name =
                        read_name(bytes, strtab.offset, strtab.size, st_name).unwrap_or_default();
                    functions.push(Symbol {
                        name,
                        value: st_value,
                        size: st_size,
                    });
                }
            }
        }

        Some(Info {
            sections: sections.into_iter().map(|(s, _, _)| s).collect(),
            functions,
        })
    }
}

// ============================================================================================
// Corpus resolution
// ============================================================================================

/// `$R2IL_CORPUS` (colon-separated) or CLI args, else the fixed fallback list: the two
/// not-stripped r2sleigh e2e binaries (symtab present, exercise Pass 2), then two stripped
/// system binaries (op-level only, exercise the Pass 2 skip line).
fn corpus_paths() -> Vec<PathBuf> {
    let args: Vec<String> = env::args().skip(1).collect();
    if !args.is_empty() {
        return args.into_iter().map(PathBuf::from).collect();
    }
    if let Ok(value) = env::var("R2IL_CORPUS") {
        let paths: Vec<PathBuf> = value
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
        if !paths.is_empty() {
            return paths;
        }
    }
    let mut out = vec![
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../r2sleigh/tests/e2e/stress_test"
        )),
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../r2sleigh/tests/e2e/stress_test_opt"
        )),
    ];
    out.push(PathBuf::from("/bin/ls"));
    out.push(PathBuf::from("/usr/bin/env"));
    out
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

// ============================================================================================
// Small, dependency-free distribution helper: mean / min / median / max over usize samples.
// ============================================================================================

fn distribution(values: &[usize]) -> Option<(f64, usize, f64, usize)> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let min = sorted[0];
    let max = sorted[sorted.len() - 1];
    let sum: usize = sorted.iter().sum();
    #[allow(clippy::cast_precision_loss)]
    let mean = sum as f64 / sorted.len() as f64;
    let mid = sorted.len() / 2;
    #[allow(clippy::cast_precision_loss)]
    let median = if sorted.len() % 2 == 1 {
        sorted[mid] as f64
    } else {
        (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0
    };
    Some((mean, min, median, max))
}

fn print_distribution(label: &str, values: &[usize]) {
    match distribution(values) {
        Some((mean, min, median, max)) => println!(
            "    {label}: mean={mean:.2} min={min} median={median:.1} max={max} n={}",
            values.len()
        ),
        None => println!("    {label}: (no samples)"),
    }
}

#[allow(clippy::cast_precision_loss)]
fn percent(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64) * 100.0
    }
}

fn is_atomic(op: &R2ILOp) -> bool {
    matches!(
        op,
        R2ILOp::AtomicCAS { .. }
            | R2ILOp::LoadLinked { .. }
            | R2ILOp::StoreConditional { .. }
            | R2ILOp::LoadGuarded { .. }
            | R2ILOp::StoreGuarded { .. }
    )
}

// ============================================================================================
// Pass 1 — MEASURED EXACT (op level)
// ============================================================================================

#[derive(Default)]
struct Pass1Stats {
    instructions_decoded: usize,
    undecodable_instructions: usize,
    ops_total: usize,
    opcode_freq: BTreeMap<&'static str, usize>,
    ops_per_instruction: Vec<usize>,
    input_arity_hist: BTreeMap<usize, usize>,
    output_count_hist: BTreeMap<usize, usize>,
    memory_ops: usize,
    control_ops: usize,
    atomic_ops: usize,
    call_other_arity: Vec<usize>,
    inline_fit: usize,
    needs_vec_routing: usize,
}

impl Pass1Stats {
    fn record_block(&mut self, block: &R2ILBlock) {
        self.ops_per_instruction.push(block.ops.len());
        for op in &block.ops {
            self.ops_total += 1;
            *self
                .opcode_freq
                .entry(OpTag::from_r2il(op).as_str())
                .or_insert(0) += 1;

            let input_arity = op.inputs().len();
            *self.input_arity_hist.entry(input_arity).or_insert(0) += 1;

            let output_count = usize::from(op.output().is_some());
            *self.output_count_hist.entry(output_count).or_insert(0) += 1;

            if op.is_memory_read() || op.is_memory_write() {
                self.memory_ops += 1;
            }
            if op.is_control_flow() {
                self.control_ops += 1;
            }
            if is_atomic(op) {
                self.atomic_ops += 1;
            }
            if let R2ILOp::CallOther { inputs, .. } = op {
                self.call_other_arity.push(inputs.len());
            }

            if output_count <= 1 && input_arity <= 2 {
                self.inline_fit += 1;
            } else {
                self.needs_vec_routing += 1;
            }
        }
    }
}

/// Build a `MIN_LIFT_BYTES`-long window starting at `bytes[offset..]`, zero-padded if the
/// section runs out of bytes before then.
fn lift_window(bytes: &[u8], offset: usize) -> Vec<u8> {
    let avail = &bytes[offset..];
    if avail.len() >= MIN_LIFT_BYTES {
        avail[..MIN_LIFT_BYTES].to_vec()
    } else {
        let mut window = avail.to_vec();
        window.resize(MIN_LIFT_BYTES, 0);
        window
    }
}

/// Linear op-level sweep of one section's bytes, capped at `cap` bytes. On `Ok`, advances by
/// `block.size.max(1)`; on `Err`, counts one undecodable instruction and advances one byte.
fn pass1_sweep(disasm: &Disassembler, bytes: &[u8], base_addr: u64, cap: usize, stats: &mut Pass1Stats) {
    let limit = bytes.len().min(cap);
    let mut offset = 0usize;
    while offset < limit {
        let addr = base_addr.saturating_add(offset as u64);
        let window = lift_window(bytes, offset);
        match disasm.lift(&window, addr) {
            Ok(block) => {
                let advance = (block.size as usize).max(1);
                stats.record_block(&block);
                stats.instructions_decoded += 1;
                offset += advance;
            }
            Err(_) => {
                stats.undecodable_instructions += 1;
                offset += 1;
            }
        }
    }
}

fn print_pass1(stats: &Pass1Stats) {
    println!("  == MEASURED EXACT (op level) ==");
    println!(
        "    instructions_decoded={} undecodable_instructions={} ops_total={}",
        stats.instructions_decoded, stats.undecodable_instructions, stats.ops_total
    );

    print!("    opcode_freq:");
    if stats.opcode_freq.is_empty() {
        println!(" (none)");
    } else {
        println!();
        for (name, count) in &stats.opcode_freq {
            println!("      {name}={count}");
        }
    }

    print_distribution("ops_per_native_instruction", &stats.ops_per_instruction);

    print!("    input_arity_histogram:");
    if stats.input_arity_hist.is_empty() {
        println!(" (none)");
    } else {
        println!();
        for (arity, count) in &stats.input_arity_hist {
            println!("      arity={arity} count={count}");
        }
    }

    print!("    output_count_histogram:");
    if stats.output_count_hist.is_empty() {
        println!(" (none)");
    } else {
        println!();
        for (outputs, count) in &stats.output_count_hist {
            println!("      outputs={outputs} count={count}");
        }
    }

    println!(
        "    memory-op %={:.2} control-op %={:.2} atomic %={:.2} (of ops_total={})",
        percent(stats.memory_ops, stats.ops_total),
        percent(stats.control_ops, stats.ops_total),
        percent(stats.atomic_ops, stats.ops_total),
        stats.ops_total
    );

    print_distribution("call_other_arity", &stats.call_other_arity);

    println!(
        "    % fitting dst+src0+src1 inline={:.2} % needing Vec routing={:.2}",
        percent(stats.inline_fit, stats.ops_total),
        percent(stats.needs_vec_routing, stats.ops_total)
    );
}

// ============================================================================================
// Pass 2 — HEURISTIC-DERIVED (function / CFG level)
// ============================================================================================

/// One lifted native instruction inside a function, kept just long enough to compute leaders.
struct InstrInfo {
    addr: u64,
    size: usize,
    is_control_flow: bool,
    /// Constant branch targets this instruction's control-flow op(s) named — the approximate
    /// half of the leader set (indirect targets are never resolved here).
    const_targets: Vec<u64>,
}

/// Sweep `[fn_start, fn_end)` inside one section's bytes at the instruction level (mirrors
/// `pass1_sweep`'s window/advance rule) to discover instruction boundaries and constant
/// control-flow targets. `section_bytes`/`section_addr` describe the CAPPED section slice this
/// function's bytes must fall within.
fn sweep_function_instructions(
    disasm: &Disassembler,
    section_bytes: &[u8],
    section_addr: u64,
    fn_start: u64,
    fn_end: u64,
) -> Vec<InstrInfo> {
    let mut infos = Vec::new();
    let mut addr = fn_start;
    while addr < fn_end {
        let Some(sec_off) = addr.checked_sub(section_addr) else {
            break;
        };
        let Ok(sec_off) = usize::try_from(sec_off) else {
            break;
        };
        if sec_off >= section_bytes.len() {
            break;
        }
        let window = lift_window(section_bytes, sec_off);
        match disasm.lift(&window, addr) {
            Ok(block) => {
                let size = (block.size as usize).max(1);
                let is_control_flow = block.ops.iter().any(R2ILOp::is_control_flow);
                let mut const_targets = Vec::new();
                for op in &block.ops {
                    if !op.is_control_flow() {
                        continue;
                    }
                    for input in op.inputs() {
                        if input.space == SpaceId::Const {
                            const_targets.push(input.offset);
                        }
                    }
                }
                infos.push(InstrInfo {
                    addr,
                    size,
                    is_control_flow,
                    const_targets,
                });
                addr = addr.saturating_add(size as u64);
            }
            Err(_) => {
                infos.push(InstrInfo {
                    addr,
                    size: 1,
                    is_control_flow: false,
                    const_targets: Vec::new(),
                });
                addr = addr.saturating_add(1);
            }
        }
    }
    infos
}

/// leaders = `{fn_start}` ∪ intra-function const branch targets ∪ `{addr after any
/// control-flow instruction}` — the approximate half of the labelling rule.
fn compute_leaders(fn_start: u64, fn_end: u64, infos: &[InstrInfo]) -> Vec<u64> {
    let mut leaders = std::collections::BTreeSet::new();
    leaders.insert(fn_start);
    for info in infos {
        for &target in &info.const_targets {
            if target >= fn_start && target < fn_end {
                leaders.insert(target);
            }
        }
        if info.is_control_flow {
            let after = info.addr.saturating_add(info.size as u64);
            if after > fn_start && after < fn_end {
                leaders.insert(after);
            }
        }
    }
    leaders.into_iter().collect()
}

/// Basic blocks = maximal ranges between leaders, the last one running to `fn_end`.
fn basic_block_ranges(fn_end: u64, leaders: &[u64]) -> Vec<(u64, u64)> {
    let mut ranges = Vec::with_capacity(leaders.len());
    for (index, &start) in leaders.iter().enumerate() {
        let end = leaders.get(index + 1).copied().unwrap_or(fn_end);
        if end > start {
            ranges.push((start, end));
        }
    }
    ranges
}

/// One `disasm.lift_block(&padded, bb_addr, bb_len)` per basic block range.
fn lift_blocks(
    disasm: &Disassembler,
    section_bytes: &[u8],
    section_addr: u64,
    ranges: &[(u64, u64)],
) -> Option<Vec<R2ILBlock>> {
    let mut out = Vec::with_capacity(ranges.len());
    for &(bb_addr, bb_end) in ranges {
        let bb_len = usize::try_from(bb_end - bb_addr).ok()?;
        let sec_off = usize::try_from(bb_addr.checked_sub(section_addr)?).ok()?;
        if sec_off >= section_bytes.len() {
            return None;
        }
        let avail_len = (section_bytes.len() - sec_off).min(bb_len);
        let mut padded = section_bytes[sec_off..sec_off + avail_len].to_vec();
        if padded.len() < MIN_LIFT_BYTES {
            padded.resize(MIN_LIFT_BYTES, 0);
        }
        match disasm.lift_block(&padded, bb_addr, bb_len) {
            Ok(block) => out.push(block),
            Err(_) => return None,
        }
    }
    Some(out)
}

#[derive(Default)]
struct Pass2Stats {
    functions_considered: usize,
    functions_processed: usize,
    functions_skipped: usize,
    blocks_per_fn: Vec<usize>,
    ops_per_block: Vec<usize>,
    phi_fanin: Vec<usize>,
    values_per_fn: Vec<usize>,
    call_sites_per_fn: Vec<usize>,
    predicates_per_fn: Vec<usize>,
    vocab_unique_ssa_names: Vec<usize>,
    vocab_unique_op_spaces: Vec<usize>,
    vocab_unique_object_spaces: Vec<usize>,
    vocab_unique_userops: Vec<usize>,
    vocab_userop_mentions: Vec<usize>,
    vocab_unique_custom_spaces_from_strings: Vec<usize>,
    vocab_total_values: Vec<usize>,
    vocab_ssa_name_bytes: Vec<usize>,
    vocab_interned_id_bytes: Vec<usize>,
    facet_ok: usize,
    facet_unknown_custom_space: usize,
    facet_ordinal_exhausted: usize,
    smelt_harvested: usize,
    smelt_classified: usize,
    smelt_residual: usize,
    smelt_dropped: usize,
}

/// Record one skipped `STT_FUNC` symbol — named, addressed, never a silent omission — and bump
/// the ledger. `symbol.name` may be empty (an unreadable/absent strtab entry); that is reported
/// as `<unnamed>` rather than left blank.
fn skip_function(stats: &mut Pass2Stats, symbol: &elf::Symbol, reason: &str) {
    let label = if symbol.name.is_empty() {
        "<unnamed>"
    } else {
        symbol.name.as_str()
    };
    println!("    skipped {label} @0x{:x}: {reason}", symbol.value);
    stats.functions_skipped += 1;
}

/// Everything Pass 2 needs that does not change per binary: the disassembler, the arch spec
/// used to build it, the pass-1-seven-opcode convention, and the two echoed caps.
struct Setup {
    disasm: Disassembler,
    spec: ArchSpec,
    conv: R2ilConvention,
    max_funcs: usize,
    max_section_bytes: usize,
}

fn print_pass2(stats: &Pass2Stats) {
    println!("  == HEURISTIC-DERIVED (function / CFG level) ==");
    println!(
        "    function_boundaries: symtab (exact) | leaders: intra-function const targets (approximate)"
    );
    println!(
        "    functions_considered={} functions_processed={} functions_skipped={}",
        stats.functions_considered, stats.functions_processed, stats.functions_skipped
    );
    print_distribution("blocks_per_fn", &stats.blocks_per_fn);
    print_distribution("ops_per_block", &stats.ops_per_block);
    print_distribution("phi_fanin", &stats.phi_fanin);
    print_distribution("values_per_fn", &stats.values_per_fn);
    print_distribution("call_sites_per_fn", &stats.call_sites_per_fn);
    print_distribution("predicates_per_fn", &stats.predicates_per_fn);

    print_distribution("vocab.unique_ssa_names", &stats.vocab_unique_ssa_names);
    print_distribution("vocab.unique_op_spaces", &stats.vocab_unique_op_spaces);
    print_distribution(
        "vocab.unique_object_spaces",
        &stats.vocab_unique_object_spaces,
    );
    print_distribution("vocab.unique_userops", &stats.vocab_unique_userops);
    print_distribution("vocab.userop_mentions", &stats.vocab_userop_mentions);
    print_distribution(
        "vocab.unique_custom_spaces_from_strings",
        &stats.vocab_unique_custom_spaces_from_strings,
    );
    print_distribution("vocab.total_values", &stats.vocab_total_values);
    print_distribution("vocab.ssa_name_bytes", &stats.vocab_ssa_name_bytes);
    print_distribution("vocab.interned_id_bytes", &stats.vocab_interned_id_bytes);

    let facet_total = stats.facet_ok + stats.facet_unknown_custom_space + stats.facet_ordinal_exhausted;
    println!(
        "    facet::project sweep: ok={} FacetOverflow(UnknownCustomSpace)={} FacetOverflow(CustomOrdinalExhausted)={} total={}",
        stats.facet_ok, stats.facet_unknown_custom_space, stats.facet_ordinal_exhausted, facet_total
    );

    let conserved = stats.smelt_dropped == 0
        && stats.smelt_harvested == stats.smelt_classified + stats.smelt_residual;
    println!(
        "    furnace::smelt conservation: harvested {} / classified {} / residual {} / dropped {} (conserved: {})",
        stats.smelt_harvested,
        stats.smelt_classified,
        stats.smelt_residual,
        stats.smelt_dropped,
        conserved
    );
}

// ============================================================================================
// Per-binary orchestration
// ============================================================================================

fn process_binary(setup: &Setup, path: &Path) {
    println!("== binary: {} ==", path.display());

    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) => {
            println!("  skipped: cannot read file ({err})");
            println!();
            return;
        }
    };

    let Some(info) = elf::parse(&bytes) else {
        println!("  skipped: not a recognized ELF64 LE x86-64 binary (malformed or wrong arch)");
        println!();
        return;
    };

    let exec_sections: Vec<&elf::Section> = info.sections.iter().filter(|s| s.is_exec()).collect();
    if exec_sections.is_empty() {
        println!("  skipped: no executable sections found");
        println!();
        return;
    }

    print!("  exec sections:");
    for section in &exec_sections {
        print!(" {}(size={})", section.name, section.size);
    }
    println!();

    let mut p1 = Pass1Stats::default();
    for section in &exec_sections {
        let Ok(start) = usize::try_from(section.offset) else {
            continue;
        };
        let len = usize::try_from(section.size).unwrap_or(0);
        let Some(section_bytes) = bytes.get(start..start.saturating_add(len)) else {
            continue;
        };
        pass1_sweep(&setup.disasm, section_bytes, section.addr, setup.max_section_bytes, &mut p1);
    }
    print_pass1(&p1);
    println!();

    if info.functions.is_empty() {
        println!("  function-level stats: skipped (no symtab)");
        println!();
        return;
    }

    let exec_sections_owned: Vec<elf::Section> = info
        .sections
        .into_iter()
        .filter(elf::Section::is_exec)
        .collect();
    let p2 = run_pass2_over(setup, &bytes, &exec_sections_owned, &info.functions);
    print_pass2(&p2);
    println!();
}

/// The real Pass 2 driver — reads section bytes out of the whole-file `bytes` buffer itself
/// (rather than pre-sliced section byte vectors), so each function's basic blocks can be lifted
/// straight from the file without an intermediate copy of the whole section.
fn run_pass2_over(
    setup: &Setup,
    bytes: &[u8],
    exec_sections: &[elf::Section],
    functions: &[elf::Symbol],
) -> Pass2Stats {
    let mut stats = Pass2Stats {
        functions_considered: functions.len(),
        ..Pass2Stats::default()
    };

    let mut sorted: Vec<&elf::Symbol> = functions.iter().collect();
    sorted.sort_by_key(|symbol| symbol.value);

    for symbol in sorted.into_iter().take(setup.max_funcs) {
        let Some(section) = exec_sections
            .iter()
            .find(|s| symbol.value >= s.addr && symbol.value < s.end_addr())
        else {
            skip_function(&mut stats, symbol, "no containing executable section");
            continue;
        };
        let Ok(sec_start) = usize::try_from(section.offset) else {
            skip_function(&mut stats, symbol, "section offset does not fit usize");
            continue;
        };
        let full_len = usize::try_from(section.size).unwrap_or(0);
        let capped_len = full_len.min(setup.max_section_bytes);
        let Some(section_bytes) = bytes.get(sec_start..sec_start.saturating_add(capped_len)) else {
            skip_function(&mut stats, symbol, "section bytes out of file bounds");
            continue;
        };

        let fn_start = symbol.value;
        let section_capped_end = section.addr.saturating_add(section_bytes.len() as u64);
        let fn_end = symbol
            .value
            .saturating_add(symbol.size)
            .min(section.end_addr())
            .min(section_capped_end);
        if fn_end <= fn_start {
            skip_function(&mut stats, symbol, "empty range after capping to section bytes");
            continue;
        }

        let infos = sweep_function_instructions(&setup.disasm, section_bytes, section.addr, fn_start, fn_end);
        let leaders = compute_leaders(fn_start, fn_end, &infos);
        let ranges = basic_block_ranges(fn_end, &leaders);
        let Some(bbs) = lift_blocks(&setup.disasm, section_bytes, section.addr, &ranges) else {
            skip_function(&mut stats, symbol, "lift_block failed on a basic block");
            continue;
        };
        let Some(behavior) = FunctionBehavior::from_blocks_raw(&bbs, Some(&setup.spec)) else {
            skip_function(&mut stats, symbol, "FunctionBehavior::from_blocks_raw returned None");
            continue;
        };

        stats.functions_processed += 1;
        stats.blocks_per_fn.push(bbs.len());
        for block in behavior.control().blocks() {
            stats.ops_per_block.push(block.ops.len());
            for phi in &block.phis {
                stats.phi_fanin.push(phi.sources.len());
            }
        }
        stats.values_per_fn.push(behavior.values().values.len());
        stats.call_sites_per_fn.push(behavior.calls().by_id.len());
        stats.predicates_per_fn.push(behavior.predicates().predicates.len());

        let harvest = VocabHarvest::from_behavior(&behavior);
        let vstats = harvest.stats();
        stats.vocab_unique_ssa_names.push(vstats.unique_ssa_names);
        stats.vocab_unique_op_spaces.push(vstats.unique_op_spaces);
        stats
            .vocab_unique_object_spaces
            .push(vstats.unique_object_spaces);
        stats.vocab_unique_userops.push(vstats.unique_userops);
        stats.vocab_userop_mentions.push(vstats.userop_mentions);
        stats
            .vocab_unique_custom_spaces_from_strings
            .push(vstats.unique_custom_spaces_from_strings);
        stats.vocab_total_values.push(vstats.total_values);
        stats.vocab_ssa_name_bytes.push(vstats.ssa_name_bytes);
        stats
            .vocab_interned_id_bytes
            .push(vstats.interned_id_bytes);

        for block in &bbs {
            for op in &block.ops {
                let mut varnodes = op.inputs();
                if let Some(output) = op.output() {
                    varnodes.push(output);
                }
                for vn in varnodes {
                    match facet::project(vn, setup.conv.spaces()) {
                        Ok(_) => stats.facet_ok += 1,
                        Err(facet::FacetOverflow::UnknownCustomSpace { .. }) => {
                            stats.facet_unknown_custom_space += 1;
                        }
                        Err(facet::FacetOverflow::CustomOrdinalExhausted { .. }) => {
                            stats.facet_ordinal_exhausted += 1;
                        }
                    }
                }
            }
        }

        let (_rows, _ledger, report) = furnace::smelt(&behavior, &bbs, &setup.conv);
        stats.smelt_harvested += report.harvested;
        stats.smelt_classified += report.classified;
        stats.smelt_residual += report.residual;
        stats.smelt_dropped += report.dropped;
    }

    stats
}

// ============================================================================================
// main
// ============================================================================================

fn main() {
    let max_funcs = env_usize("R2IL_PROFILE_MAX_FUNCS", DEFAULT_MAX_FUNCS);
    let max_section_bytes = env_usize("R2IL_PROFILE_MAX_SECTION_BYTES", DEFAULT_MAX_SECTION_BYTES);

    println!("r2il corpus profile (§12)");
    println!(
        "caps: R2IL_PROFILE_MAX_FUNCS={max_funcs} R2IL_PROFILE_MAX_SECTION_BYTES={max_section_bytes}"
    );
    println!(
        "function_boundaries: symtab (exact) | leaders: intra-function const targets (approximate)"
    );
    println!();

    let spec = match build_arch_spec(
        sleigh_config::processor_x86::SLA_X86_64,
        sleigh_config::processor_x86::PSPEC_X86_64,
        "x86-64",
    ) {
        Ok(spec) => spec,
        Err(err) => {
            println!("FATAL: build_arch_spec(x86-64) failed: {err}");
            return;
        }
    };

    let mut disasm = match Disassembler::from_sla(
        sleigh_config::processor_x86::SLA_X86_64,
        sleigh_config::processor_x86::PSPEC_X86_64,
        "x86-64",
    ) {
        Ok(disasm) => disasm,
        Err(err) => {
            println!("FATAL: Disassembler::from_sla(x86-64) failed: {err}");
            return;
        }
    };
    disasm.set_userop_map(userop_map_for_arch("x86-64"));

    // The pass-1 seven, bootstrapped from the real x86-64 ArchSpec so Pass 2's smelt/facet
    // sweep can also resolve register operands (see impl-spec §10 test 12). Falls back to the
    // unbootstrapped `minimal_pass_one` convention (still classifying the same seven opcodes,
    // just without register rows or a CustomSpaceTable) if the arch's own custom-space count
    // ever overflowed the lo-u16 budget — practically unreachable for x86-64, handled anyway
    // rather than assumed away.
    let seven = [
        OpTag::Copy,
        OpTag::IntAdd,
        OpTag::Load,
        OpTag::Store,
        OpTag::CBranch,
        OpTag::Call,
        OpTag::Return,
    ];
    let conv = match R2ilConvention::from_arch(&spec, seven) {
        Ok(conv) => conv,
        Err(err) => {
            println!(
                "WARN: R2ilConvention::from_arch(x86-64) failed ({err}); falling back to minimal_pass_one"
            );
            R2ilConvention::minimal_pass_one()
        }
    };

    let setup = Setup {
        disasm,
        spec,
        conv,
        max_funcs,
        max_section_bytes,
    };

    for path in corpus_paths() {
        process_binary(&setup, &path);
    }
}
