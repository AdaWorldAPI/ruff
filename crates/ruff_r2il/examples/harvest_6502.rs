//! THE 6502 pass-1 harvest — Probe C's intake leg, run against real C64 program images.
//!
//! Same pipeline as `harvest_r2il.rs` (x86-64 ELF corpus), different front door: a C64 `.PRG`
//! image carries its own 2-byte little-endian load address, has no symtab and no sections, so
//! each image is harvested as ONE function spanning the whole resident range. The 6502 SLEIGH
//! spec is compiled AT RUN TIME by `sleigh-compiler` (dev-dependency; the same crate
//! `C64-rs/crates/c64-lift/build.rs` uses at build time) from the unmodified Ghidra
//! `6502.slaspec` in a sibling checkout — this crate vendors nothing.
//!
//! Corpus default: the Elite reference binaries in the sibling
//! `adaworldapi/elite-source-code-commodore-64` checkout (`gma4/5/6.bin` = COMLOD / LOCODE /
//! HICODE, each with its PRG load-address header). **Licensing posture (operator-ruled,
//! 2026-08-24):** the corpus is read locally and never redistributed — artifacts carry FNV
//! hashes and counts only, never the bytes. A missing corpus file is a printed skip, never a
//! failure, so this example is runnable on any checkout.
//!
//! Artifacts land in `.claude/harvest/r2il-6502/` (override `R2IL_6502_HARVEST_OUT`) with the
//! same shapes as the x86 harvest: ore.tsv / census.md / slag.tsv / convention.toml /
//! PROVENANCE.md. **Evidence, never a re-ingest path — nothing in ruff parses them back.**
//!
//! Run:
//! ```sh
//! cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example harvest_6502
//! ```

// Same policy note as `harvest_r2il.rs`: the workspace clippy disallow-list on `std::fs`/
// `std::env::var` targets ty crates routing through `System`; this workspace-EXCLUDED crate has
// no `System`, and the writes below ARE the deliverable.
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
use sleigh_compiler::{SleighCompiler, SleighCompilerOptions};

use ruff_r2il::behavior::FunctionBehavior;
use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::facet::FacetPrefix;
use ruff_r2il::furnace::{self, Census, FlatFact, HarvestReport};
use ruff_r2il::ore::OpTag;

// ================================================================================================
// FNV-1a 64 (labelled FNV — never a cryptographic hash) + the pass-1 seven.
// Duplicated (not shared) against harvest_r2il.rs — examples are standalone by design.
// ================================================================================================

const FNV_OFFSET_BASIS_64: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME_64: u64 = 0x0000_0100_0000_01b3;

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET_BASIS_64;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME_64);
    }
    hash
}

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

/// Minimum bytes libsla needs per lift call (mirrors the private
/// `Disassembler::MIN_BYTES`) — every window is zero-padded up to this length.
const LIFT_MIN_BYTES: usize = 16;

fn pad_window(bytes: &[u8], want: usize) -> Vec<u8> {
    let mut w = bytes.to_vec();
    if w.len() < want {
        w.resize(want, 0);
    }
    w
}

type ReasonBucket = BTreeMap<(u64, &'static str), (usize, Option<[u8; 16]>)>;
type AddrBucket = BTreeMap<(Option<FacetPrefix>, u64), usize>;

// ================================================================================================
// Spec + corpus location — sibling checkouts, env-overridable, skip-if-absent.
// ================================================================================================

/// The unmodified Ghidra 6502 slaspec, from the C64-rs vendored copy (Apache-2.0, provenance in
/// its `NOTICE.md`). Overridable so a checkout that has the Ghidra tree itself can point there.
fn slaspec_path() -> PathBuf {
    env::var("C64_SLASPEC")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../c64-rs/vendor/ghidra-6502/data/languages/6502.slaspec"
            ))
        })
}

/// Default corpus: the PRG-headed Elite images (gma4=COMLOD, gma5=LOCODE, gma6=HICODE).
/// `C64_PRG` (colon-separated paths) overrides.
fn corpus_paths() -> Vec<PathBuf> {
    if let Ok(list) = env::var("C64_PRG") {
        return list
            .split(':')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect();
    }
    let base = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../adaworldapi/elite-source-code-commodore-64/4-reference-binaries/gma86-pal"
    );
    ["gma4.bin", "gma5.bin", "gma6.bin"]
        .iter()
        .map(|f| PathBuf::from(base).join(f))
        .collect()
}

/// Compile the slaspec to a `.sla` in a scratch dir and read it back. Run-time compilation is
/// deliberate: this crate vendors no spec and gains no build.rs — the spec's one source of truth
/// stays the sibling checkout.
fn compile_slaspec(input: &PathBuf) -> Result<Vec<u8>, String> {
    let out_dir = env::temp_dir().join(format!("ruff-r2il-6502-{}", std::process::id()));
    fs::create_dir_all(&out_dir).map_err(|e| format!("scratch dir: {e}"))?;
    let output = out_dir.join("6502.sla");
    let mut compiler = SleighCompiler::new(SleighCompilerOptions::default());
    let response = compiler
        .compile(input, &output)
        .map_err(|e| format!("sleigh-compiler on {}: {e}", input.display()))?;
    for warning in &response.warnings {
        eprintln!("[harvest-6502] slaspec warning: {warning}");
    }
    fs::read(&output).map_err(|e| format!("read compiled sla: {e}"))
}

// ================================================================================================
// Leader scan + basic blocks — the harvest_r2il.rs shape, unchanged in mechanism.
// ================================================================================================

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
                    // Same Ram-vs-Const finding as `find_call_targets` — branch targets
                    // on the 6502 lift are Ram-space too.
                    if let Some(t) = target
                        && matches!(t.space, SpaceId::Const | SpaceId::Ram)
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

/// Function entries on a symtab-less PRG: every `Call` (JSR) target that lands inside the
/// resident range, found by one linear sweep. This is the 6502 stand-in for `STT_FUNC` —
/// without it the whole image is one function whose SSA keeps only the entry-reachable
/// component, and the first run of this harvest measured exactly that collapse
/// (52 KB of Elite -> 38 classified facts).
fn find_call_targets(disasm: &Disassembler, code: &[u8], load_addr: u64) -> BTreeSet<u64> {
    let end = load_addr + code.len() as u64;
    let mut targets = BTreeSet::new();
    let mut offset = 0usize;
    while offset < code.len() {
        let addr = load_addr + offset as u64;
        let window = pad_window(&code[offset..], LIFT_MIN_BYTES);
        match disasm.lift(&window, addr) {
            Ok(block) => {
                for op in &block.ops {
                    // MEASURED (first run of this example): 6502 direct JSR targets lift
                    // with `SpaceId::Ram`, not `Const` — the x86 harvest's Const filter
                    // silently discarded every one of them. Accept both.
                    if let R2ILOp::Call { target } = op
                        && matches!(target.space, SpaceId::Const | SpaceId::Ram)
                        && target.offset > load_addr
                        && target.offset < end
                    {
                        targets.insert(target.offset);
                    }
                }
                offset += (block.size as usize).max(1);
            }
            Err(_) => {
                offset += 1;
            }
        }
    }
    targets
}

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
// Accumulators + rendering — harvest_r2il.rs shapes.
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
        }) => {
            format!("space:{discriminant}/offset:{offset:#x}")
        }
        Some(FacetPrefix::SpaceOffsetSize {
            discriminant,
            offset,
            size,
        }) => {
            format!("space:{discriminant}/offset:{offset:#x}/size:{size}")
        }
    }
}

fn render_ore_tsv(rows: &[OreRow]) -> String {
    let mut out = String::new();
    out.push_str(
        "# R2IL 6502 pass-1 ore — evidence, never a re-ingest path. Nothing in ruff parses this back.\n",
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
    out.push_str("# R2IL 6502 pass-1 census\n\n");
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
        "# R2IL 6502 pass-1 slag — the addressed residual ledger. Evidence, never a re-ingest path.\n",
    );
    out.push_str("#version 1\n");
    out.push_str(
        "#section grouped — ResidualLedger::grouped(), merged across every harvested image\n",
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
        "#section by_address — ResidualLedger::by_address(), the proposer's work queue, merged across every harvested image\n",
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

// ================================================================================================
// main
// ================================================================================================

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = env::var("R2IL_6502_HARVEST_OUT").unwrap_or_else(|_| {
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../.claude/harvest/r2il-6502"
        )
        .to_string()
    });
    fs::create_dir_all(&out_dir)?;
    eprintln!("[harvest-6502] output directory: {out_dir}");

    let spec_path = slaspec_path();
    if !spec_path.exists() {
        eprintln!(
            "[harvest-6502] SKIP: 6502.slaspec not found at {} (set C64_SLASPEC); nothing harvested",
            spec_path.display()
        );
        return Ok(());
    }
    let sla_bytes = compile_slaspec(&spec_path)?;
    let pspec_path = spec_path.with_extension("pspec");
    let pspec = fs::read_to_string(&pspec_path)
        .map_err(|e| format!("read {}: {e}", pspec_path.display()))?;

    let spec: ArchSpec = build_arch_spec(&sla_bytes, &pspec, "6502")?;
    let mut disasm = Disassembler::from_sla(&sla_bytes, &pspec, "6502")?;
    disasm.set_userop_map(userop_map_for_arch("6502"));

    let conv = R2ilConvention::from_arch(&spec, pass_one_seven())
        .map_err(|e| format!("R2ilConvention::from_arch(6502) failed: {e}"))?;
    eprintln!(
        "[harvest-6502] convention built from {} registers, {} userops",
        spec.registers.len(),
        spec.userops.len()
    );

    let mut entries: Vec<CorpusEntry> = Vec::new();
    let mut rows: Vec<OreRow> = Vec::new();
    let mut reason_bucket: ReasonBucket = BTreeMap::new();
    let mut addr_bucket: AddrBucket = BTreeMap::new();
    let mut combined = HarvestReport::default();

    for path in corpus_paths() {
        let label = path.file_name().map_or_else(
            || path.display().to_string(),
            |f| f.to_string_lossy().into_owned(),
        );
        let data = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!(
                    "[harvest-6502] {label}: skip — read failed ({e}); the corpus is a local sibling checkout, never committed"
                );
                entries.push(CorpusEntry {
                    path: path.display().to_string(),
                    len: 0,
                    fnv: None,
                    status: format!("skipped (read failed: {e})"),
                });
                continue;
            }
        };
        // A PRG image: 2-byte LE load address, then the resident bytes.
        if data.len() < 3 {
            eprintln!("[harvest-6502] {label}: skip — too short for a PRG header");
            entries.push(CorpusEntry {
                path: path.display().to_string(),
                len: data.len(),
                fnv: Some(fnv1a64(&data)),
                status: "skipped (too short)".to_string(),
            });
            continue;
        }
        let load_addr = u64::from(u16::from_le_bytes([data[0], data[1]]));
        let code = &data[2..];
        let fnv = fnv1a64(&data);
        eprintln!(
            "[harvest-6502] {label}: {} bytes, load ${load_addr:04x}, fnv1a64={fnv:016x}",
            code.len()
        );

        // No symtab exists on a PRG, and harvesting the whole image as ONE function loses
        // everything the SSA entry cannot reach. Instead: JSR targets partition the image —
        // each Call target is a function entry (the 6502 stand-in for STT_FUNC), each
        // partition runs from its entry to the next one.
        let call_targets = find_call_targets(&disasm, code, load_addr);
        let end = load_addr + code.len() as u64;
        let mut starts: Vec<u64> = std::iter::once(load_addr).chain(call_targets).collect();
        starts.push(end);
        starts.dedup();

        let mut img_funcs = 0usize;
        let mut img_facts = 0usize;
        let mut img_residual = 0usize;
        for w in starts.windows(2) {
            let (fn_addr, fn_end) = (w[0], w[1]);
            if fn_end <= fn_addr {
                continue;
            }
            let lo = (fn_addr - load_addr) as usize;
            let hi = (fn_end - load_addr) as usize;
            let Some(fn_code) = code.get(lo..hi) else {
                continue;
            };
            let blocks = lift_function_blocks(&disasm, fn_code, fn_addr);
            if blocks.is_empty() {
                continue;
            }
            let Some(behavior) = FunctionBehavior::from_blocks_raw(&blocks, Some(&spec)) else {
                continue;
            };
            let behavior = behavior.with_name(format!("{label}@{fn_addr:#x}"));

            let (facts, ledger, report) = furnace::smelt(&behavior, &blocks, &conv);
            combined.harvested += report.harvested;
            combined.classified += report.classified;
            combined.residual += report.residual;
            combined.dropped += report.dropped;
            img_funcs += 1;
            img_facts += facts.len();
            img_residual += report.residual;

            let func_hex = format!("{fn_addr:#x}");
            for fact in &facts {
                rows.push(OreRow {
                    binary: label.clone(),
                    func_hex: func_hex.clone(),
                    fact: *fact,
                });
            }
            for (shape_id, reason, count, example) in ledger.grouped() {
                let entry = reason_bucket
                    .entry((shape_id.0, reason))
                    .or_insert((0usize, None));
                entry.0 += count;
                if entry.1.is_none() {
                    entry.1 = example.map(|f| f.0);
                }
            }
            for (prefix, shape_id, count) in ledger.by_address() {
                *addr_bucket.entry((prefix, shape_id.0)).or_insert(0) += count;
            }
        }

        if img_funcs == 0 {
            eprintln!("[harvest-6502] {label}: no harvestable functions, skip");
            entries.push(CorpusEntry {
                path: path.display().to_string(),
                len: data.len(),
                fnv: Some(fnv),
                status: "skipped (no harvestable functions)".to_string(),
            });
            continue;
        }
        entries.push(CorpusEntry {
            path: path.display().to_string(),
            len: data.len(),
            fnv: Some(fnv),
            status: format!(
                "harvested ({img_funcs} JSR-partitioned functions, {img_facts} facts, {img_residual} residual)"
            ),
        });
    }

    eprintln!(
        "[harvest-6502] done: {} images, {} FlatFact rows, conservation {} / {} / {} / {}",
        entries.len(),
        rows.len(),
        combined.harvested,
        combined.classified,
        combined.residual,
        combined.dropped
    );

    let facts_only: Vec<FlatFact> = rows.iter().map(|r| r.fact).collect();

    fs::write(
        format!("{out_dir}/r2il-6502-pass1.ore.tsv"),
        render_ore_tsv(&rows),
    )?;
    fs::write(
        format!("{out_dir}/r2il-6502-pass1-census.md"),
        render_census_md(&facts_only),
    )?;
    fs::write(
        format!("{out_dir}/r2il-6502-pass1-slag.tsv"),
        render_slag_tsv(&reason_bucket, &addr_bucket),
    )?;
    fs::write(
        format!("{out_dir}/r2il-6502-convention.toml"),
        conv.to_toml(),
    )?;

    let mut prov = String::new();
    prov.push_str("# 6502 harvest provenance\n\n");
    prov.push_str(
        "Corpus: local sibling checkout, read-only — bytes are NEVER redistributed; this file\ncarries FNV-1a 64 hashes (labelled FNV, not a sha) and counts only.\n\n",
    );
    prov.push_str(&format!(
        "Spec: {} (unmodified Ghidra 6502 SLEIGH, Apache-2.0)\n\n",
        spec_path.display()
    ));
    prov.push_str("| image | bytes | fnv1a64 | status |\n|---|---|---|---|\n");
    for e in &entries {
        let fnv = e
            .fnv
            .map_or_else(|| "-".to_string(), |v| format!("{v:016x}"));
        prov.push_str(&format!(
            "| {} | {} | {fnv} | {} |\n",
            e.path, e.len, e.status
        ));
    }
    prov.push_str(&format!(
        "\nConservation (harvested / classified / residual / dropped): {} / {} / {} / {}\n",
        combined.harvested, combined.classified, combined.residual, combined.dropped
    ));
    fs::write(format!("{out_dir}/PROVENANCE.md"), prov)?;
    eprintln!("[harvest-6502] artifacts written to {out_dir}");

    Ok(())
}
