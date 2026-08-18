//! The round-trip reconstruction oracle over a real corpus — plain text to stdout, no file
//! writes (`.claude/plans/r2il-roundtrip-oracle-spec-v1.md` §3.7, ratified v3).
//!
//! For each binary: a linear byte sweep of every executable section lifts native instructions
//! into `R2ILBlock`s (the same `Disassembler::lift`-per-instruction pass `r2il_corpus_profile`
//! calls "MEASURED EXACT (op level)"), the sweep's blocks are chunked into
//! `R2IL_ORACLE_CHUNK_BLOCKS`-sized groups, and each chunk is run through
//! `FunctionBehavior::from_blocks_raw` → `furnace::smelt` → `oracle::reconstruct` →
//! `oracle::judge` under BOTH conventions.
//!
//! **Both conventions are reported, never conflated** (spec §3.5, normative): a verdict holding
//! under `permissive_convention` proves the reconstruction MECHANISM; the `minimal_pass_one`
//! column shows what the SHIPPED default actually covers, which on a zero-row convention is
//! accounting rather than matching.
//!
//! **Labelling rule, inherited from the profiler:** the chunking is a linear-sweep
//! APPROXIMATION of function boundaries, not a symtab-exact one — a chunk is a window of
//! consecutive lifted blocks, so its CFG is whatever those blocks' own branch targets imply.
//! That is sound for the oracle (it measures round-trip fidelity of whatever CFG it is given)
//! and is NOT a claim about function decomposition. Every printed row says `chunked`.
//!
//! Nothing here is copied into ruff: the corpus lives outside the repository, and this example
//! only ever reads it. No `unwrap`/`panic` on corpus input.

// See `r2il_corpus_profile.rs`'s identical note: ruff's `clippy.toml` disallowed-methods list is
// directory-scoped and reaches this workspace-EXCLUDED crate even though its reasons say "in ty
// crates"; `ruff_r2il` has no `ty::System` to route through, and these are the documented
// `R2IL_ORACLE_*` / `R2IL_CORPUS` overrides.
#![expect(
    clippy::disallowed_methods,
    reason = "not a ty crate: `System` is unavailable to this workspace-excluded crate; these are the example's documented env overrides"
)]

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use r2il::R2ILBlock;
use r2sleigh_lift::{Disassembler, userop_map_for_arch};

use ruff_r2il::behavior::FunctionBehavior;
use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::furnace;
use ruff_r2il::oracle::{self, OracleVerdict};

/// Minimum bytes libsla needs per lift call — mirrors the profiler's own constant.
const MIN_LIFT_BYTES: usize = 16;
const DEFAULT_MAX_SECTION_BYTES: usize = 262_144;
const DEFAULT_CHUNK_BLOCKS: usize = 24;
const DEFAULT_MAX_CHUNKS: usize = 200;

const ELF_EXEC_FLAG: u64 = 0x4;
const ELF_SHT_NOBITS: u32 = 8;

/// Minimal ELF64 LE executable-section reader. Every read is bounds-checked; anything malformed
/// yields an empty list and the caller skips the binary with a printed note. Deliberately much
/// smaller than `r2il_corpus_profile`'s `mod elf` — the oracle needs no symtab.
fn exec_sections(bytes: &[u8]) -> Vec<(String, u64, u64, u64)> {
    let read_u16 = |off: usize| -> Option<u16> {
        bytes
            .get(off..off + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let read_u32 = |off: usize| -> Option<u32> {
        bytes
            .get(off..off + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let read_u64 = |off: usize| -> Option<u64> {
        bytes
            .get(off..off + 8)
            .map(|b| u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    };

    if bytes.get(0..4) != Some(&[0x7f, b'E', b'L', b'F']) || bytes.get(4) != Some(&2) {
        return Vec::new();
    }
    let (Some(shoff), Some(shentsize), Some(shnum), Some(shstrndx)) = (
        read_u64(0x28),
        read_u16(0x3a),
        read_u16(0x3c),
        read_u16(0x3e),
    ) else {
        return Vec::new();
    };
    let (shoff, shentsize, shnum, shstrndx) = (
        shoff as usize,
        shentsize as usize,
        shnum as usize,
        shstrndx as usize,
    );
    if shentsize < 64 || shnum == 0 || shstrndx >= shnum {
        return Vec::new();
    }

    let strtab_hdr = shoff + shstrndx * shentsize;
    let Some(strtab_off) = read_u64(strtab_hdr + 0x18).map(|v| v as usize) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for index in 0..shnum {
        let hdr = shoff + index * shentsize;
        let (Some(name_off), Some(sh_type), Some(flags), Some(addr), Some(offset), Some(size)) = (
            read_u32(hdr),
            read_u32(hdr + 0x04),
            read_u64(hdr + 0x08),
            read_u64(hdr + 0x10),
            read_u64(hdr + 0x18),
            read_u64(hdr + 0x20),
        ) else {
            continue;
        };
        if flags & ELF_EXEC_FLAG == 0 || sh_type == ELF_SHT_NOBITS || size == 0 {
            continue;
        }
        let name_start = strtab_off.saturating_add(name_off as usize);
        let name = bytes
            .get(name_start..)
            .and_then(|tail| tail.iter().position(|&c| c == 0).map(|end| &tail[..end]))
            .map(|raw| String::from_utf8_lossy(raw).into_owned())
            .unwrap_or_else(|| format!("sh{index}"));
        out.push((name, addr, offset, size));
    }
    out
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default)
}

/// `$R2IL_CORPUS` (colon-separated) or CLI args, else the same fallback list the §12 profiler
/// uses, so the two examples measure the same corpus.
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
    vec![
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../r2sleigh/tests/e2e/stress_test"
        )),
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../r2sleigh/tests/e2e/stress_test_opt"
        )),
        PathBuf::from("/bin/ls"),
        PathBuf::from("/usr/bin/env"),
    ]
}

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

/// Accumulated verdict counters across every chunk, per convention.
#[derive(Default)]
struct Totals {
    chunks: usize,
    holds: usize,
    matched: usize,
    ledger_accounted: usize,
    ssa_only_residuals: usize,
    orphans: usize,
    mismatches: usize,
    gaps: BTreeMap<&'static str, usize>,
}

impl Totals {
    fn absorb(&mut self, verdict: &OracleVerdict) {
        self.chunks += 1;
        if verdict.holds() {
            self.holds += 1;
        }
        self.matched += verdict.matched;
        self.ledger_accounted += verdict.ledger_accounted;
        self.ssa_only_residuals += verdict.ssa_only_residuals;
        self.orphans += verdict.orphans.len();
        self.mismatches += verdict.mismatches.len();
        for gap in &verdict.attribute_gaps {
            *self.gaps.entry(gap.attribute.as_str()).or_insert(0) += 1;
        }
    }

    fn print(&self, label: &str) {
        println!(
            "    {label}: chunks={} holds={}/{} matched={} ledger_accounted={} \
             ssa_only_residuals={} orphans={} mismatches={}",
            self.chunks,
            self.holds,
            self.chunks,
            self.matched,
            self.ledger_accounted,
            self.ssa_only_residuals,
            self.orphans,
            self.mismatches,
        );
        print!("      attribute_gap_census:");
        if self.gaps.is_empty() {
            println!(" (none)");
        } else {
            println!();
            for (name, count) in &self.gaps {
                println!("        {name}={count}");
            }
        }
    }
}

fn run_chunk(blocks: &[R2ILBlock], conv: &R2ilConvention, totals: &mut Totals) {
    let Some(behavior) = FunctionBehavior::from_blocks_raw(blocks, None) else {
        return;
    };
    let (rows, ledger, report) = furnace::smelt(&behavior, blocks, conv);
    if !report.is_conserved() {
        // Conservation is the furnace's own invariant (plan C3); a violation makes every
        // downstream number meaningless, so it is reported rather than absorbed.
        println!("    WARNING: conservation failed on a chunk — verdict omitted");
        return;
    }
    let recon = oracle::reconstruct(&rows, conv.spaces());
    totals.absorb(&oracle::judge(blocks, &recon, &ledger));
}

fn process_binary(
    disasm: &Disassembler,
    path: &Path,
    max_section_bytes: usize,
    chunk: usize,
    max_chunks: usize,
) {
    println!("== binary: {} ==", path.display());

    let Ok(bytes) = fs::read(path) else {
        println!("  skipped: cannot read file");
        println!();
        return;
    };
    let sections = exec_sections(&bytes);
    if sections.is_empty() {
        println!("  skipped: no executable sections in a recognized ELF64 LE image");
        println!();
        return;
    }

    let mut lifted: Vec<R2ILBlock> = Vec::new();
    let mut undecodable = 0usize;
    for (_, addr, offset, size) in &sections {
        let (Ok(start), Ok(len)) = (usize::try_from(*offset), usize::try_from(*size)) else {
            continue;
        };
        let Some(section_bytes) = bytes.get(start..start.saturating_add(len)) else {
            continue;
        };
        let limit = section_bytes.len().min(max_section_bytes);
        let mut cursor = 0usize;
        while cursor < limit {
            let at = addr.saturating_add(cursor as u64);
            match disasm.lift(&lift_window(section_bytes, cursor), at) {
                Ok(block) => {
                    cursor += (block.size as usize).max(1);
                    lifted.push(block);
                }
                Err(_) => {
                    undecodable += 1;
                    cursor += 1;
                }
            }
        }
    }

    let chunks: Vec<&[R2ILBlock]> = lifted.chunks(chunk.max(1)).take(max_chunks).collect();
    // Always measured and printed, because it is what EXPLAINS the orphan count below. A
    // linear-sweep window's blocks branch to targets outside the window, so `CFG::from_blocks`
    // drops every block unreachable from the chunk's entry; those blocks' ops never reach
    // `ore::enumerate` at all and therefore have neither a fact row nor a residual — they land
    // in `orphans` by construction. Reporting the ratio here keeps that visible instead of
    // letting a reader mistake a chunking artifact for a reconstruction defect.
    let mut chunk_source_blocks = 0usize;
    let mut chunk_cfg_blocks = 0usize;
    for window in &chunks {
        chunk_source_blocks += window.len();
        if let Some(behavior) = FunctionBehavior::from_blocks_raw(window, None) {
            chunk_cfg_blocks += behavior.control().num_blocks();
        }
    }
    println!(
        "  lifted_blocks={} undecodable={} chunked_windows={} (chunk={} blocks, linear sweep — \
         an APPROXIMATION of function boundaries, never a symtab-exact claim)",
        lifted.len(),
        undecodable,
        chunks.len(),
        chunk,
    );
    println!(
        "  chunk_blocks_reaching_the_cfg={chunk_cfg_blocks}/{chunk_source_blocks} — the rest are \
         unreachable from their chunk's entry, produce NO ore facts, and are therefore counted \
         as orphans below (a chunking artifact, not a reconstruction defect)"
    );

    let mut permissive_totals = Totals::default();
    let mut minimal_totals = Totals::default();
    let minimal = R2ilConvention::minimal_pass_one();

    for window in chunks {
        if let Ok(conv) = oracle::permissive_convention(window) {
            run_chunk(window, &conv, &mut permissive_totals);
        }
        run_chunk(window, &minimal, &mut minimal_totals);
    }

    permissive_totals.print("permissive (MECHANISM proof)");
    minimal_totals.print("minimal_pass_one (SHIPPED default coverage)");
    println!();
}

fn main() {
    let max_section_bytes = env_usize("R2IL_ORACLE_MAX_SECTION_BYTES", DEFAULT_MAX_SECTION_BYTES);
    let chunk = env_usize("R2IL_ORACLE_CHUNK_BLOCKS", DEFAULT_CHUNK_BLOCKS);
    let max_chunks = env_usize("R2IL_ORACLE_MAX_CHUNKS", DEFAULT_MAX_CHUNKS);

    println!("r2il round-trip reconstruction oracle");
    println!(
        "caps: R2IL_ORACLE_MAX_SECTION_BYTES={max_section_bytes} \
         R2IL_ORACLE_CHUNK_BLOCKS={chunk} R2IL_ORACLE_MAX_CHUNKS={max_chunks}"
    );
    println!(
        "BOTH conventions are reported and never conflated: permissive proves the reconstruction \
         MECHANISM; minimal_pass_one shows the SHIPPED default's coverage."
    );
    println!();

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

    for path in corpus_paths() {
        process_binary(&disasm, &path, max_section_bytes, chunk, max_chunks);
    }
}
