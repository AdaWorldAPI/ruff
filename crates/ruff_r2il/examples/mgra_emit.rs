//! Emit the 6502 reachability partition as an MGRA v3 graph the `a2ui-graph` field
//! renderer can draw.
//!
//! `reachability_probe` measures the partition and prints a table: `CODE [29.9 .. 59.9]`
//! reached against `DATA [0.0 .. 0.9]`, disjoint, a 33x gap. That partition is a GRAPH —
//! addresses are nodes, calls and data references are edges, reached/unreached is a colour
//! — and the display tier for exactly this wire already exists and already ships
//! (`medcare-rs` emits it from `graph_abi.rs`, `a2ui-graph` draws it over wgpu with a
//! WebGL2 fallback). This example is the missing emitter, not a new renderer.
//!
//! # Why the walk is here and not shared with the probe
//!
//! The probe's `walk` records call TARGETS (`BTreeSet<u64>`); a graph needs call EDGES —
//! which function the call came from. That is one extra piece of state threaded through the
//! queue, and the two walks are otherwise the same descent. They are kept separate
//! deliberately: the probe's numbers are pinned in `CLAUDE.md` and in a merged PR, and
//! rewriting its walk to serve a second consumer is how a measured result quietly moves.
//! If a third consumer appears, extract then — with the probe's numbers re-run as the
//! falsifier that the extraction changed nothing.
//!
//! # What "origin" means, precisely
//!
//! Every queue entry carries the address the descent STARTED from, and every call or data
//! reference found while walking is attributed to that origin. So an origin is a
//! *discovered entry point*, not a function in any linker's sense — the 6502 image has no
//! symbol table, and `collect_seeds` finds entries by linear sweep for call targets. A
//! block reached only by falling through from another entry is attributed to whichever
//! entry reached it first. That is a real limitation of descent without a symbol table, not
//! a modelling choice, and it is why the emitted graph is a call graph over discovered
//! entries rather than over functions.
//!
//! ```sh
//! cargo run -p ruff_r2il --features lift --example mgra_emit
//! ```

#![expect(
    clippy::disallowed_methods,
    reason = "not a ty crate: `System` is unavailable to this workspace-excluded crate; these are the emitter's env overrides and corpus reads"
)]

use std::collections::{BTreeSet, VecDeque};
use std::env;
use std::fs;
use std::path::PathBuf;

use r2il::{R2ILOp, SpaceId};
use r2sleigh_lift::{Disassembler, build_arch_spec, userop_map_for_arch};
use ruff_r2il::absref::absolute_refs;
use ruff_r2il::mgra::{EdgeKind, GraphBuilder, Palette, encode};
use sleigh_compiler::{SleighCompiler, SleighCompilerOptions};

const LIFT_MIN_BYTES: usize = 16;

struct Subject {
    file: &'static str,
    load: u64,
    out: &'static str,
}

/// The two DECRYPTED code images. The data controls the probe uses are deliberately absent:
/// a standalone data file has no callers, so its graph is N isolated nodes and no edges —
/// nothing to look at, and nothing the emitter would be tested by.
const SUBJECTS: &[Subject] = &[
    Subject {
        file: "LOCODE.unprot.bin",
        load: 0x1d00,
        out: "locode.abi",
    },
    Subject {
        file: "HICODE.unprot.bin",
        load: 0x6a00,
        out: "hicode.abi",
    },
];

fn pad(bytes: &[u8], want: usize) -> Vec<u8> {
    let mut v = bytes.to_vec();
    if v.len() < want {
        v.resize(want, 0);
    }
    v
}

/// Linear sweep for call targets — the 6502 stand-in for a symbol table.
///
/// The load address is NOT an entry point on these images (measured: `$00` = `BRK` on
/// several), so seeding descent with it alone reaches nothing. Every seed here is an address
/// some instruction somewhere calls.
fn collect_seeds(disasm: &Disassembler, code: &[u8], load: u64) -> BTreeSet<u64> {
    let end = load + code.len() as u64;
    let mut seeds = BTreeSet::new();
    let mut off = 0usize;
    while off < code.len() {
        let addr = load + off as u64;
        match disasm.lift(&pad(&code[off..], LIFT_MIN_BYTES), addr) {
            Ok(block) => {
                for op in &block.ops {
                    if let R2ILOp::Call { target } = op
                        && matches!(target.space, SpaceId::Const | SpaceId::Ram)
                        && target.offset >= load
                        && target.offset < end
                    {
                        seeds.insert(target.offset);
                    }
                }
                off += (block.size as usize).max(1);
            }
            Err(_) => off += 1,
        }
    }
    seeds
}

/// Recursive descent, attributing every call and data reference to the entry it was reached
/// from. Follows fall-through, both arms of a conditional branch, and the target of an
/// unconditional branch; stops a path at a return, an indirect transfer, or a decode failure.
fn walk_into(
    disasm: &Disassembler,
    code: &[u8],
    load: u64,
    entries: &BTreeSet<u64>,
    g: &mut GraphBuilder,
) -> usize {
    let end = load + code.len() as u64;
    let in_image = |a: u64| a >= load && a < end;

    let mut instructions = 0usize;
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    // `(address to decode, the entry this path started from)`.
    let mut queue: VecDeque<(u64, u64)> = VecDeque::new();
    for &e in entries {
        g.observe(e, Palette::Entry);
        queue.push_back((e, e));
    }

    while let Some((start, origin)) = queue.pop_front() {
        let mut addr = start;
        loop {
            if !in_image(addr) || !seen.insert(addr) {
                break;
            }
            let off = (addr - load) as usize;
            let Ok(block) = disasm.lift(&pad(&code[off..], LIFT_MIN_BYTES), addr) else {
                break;
            };
            let size = (block.size as usize).max(1);
            instructions += 1;

            // Absolute references, resolved through the block by `ruff_r2il::absref`
            // rather than read off the address operand — on 6502 the address of an
            // absolute-indexed access is the constant operand of the `IntAdd` that defines
            // the address temp. `in_image` is deliberately NOT applied: an out-of-image
            // absolute is zero page, the stack page, or a hardware register, and those are
            // real references. It would also be meaningless against a temp, whose offset is
            // a slot number rather than an address.
            for a in absolute_refs(&block.ops) {
                g.edge(origin, a, EdgeKind::AbsRef);
            }

            let mut stop = false;
            let mut redirect: Option<u64> = None;
            for op in &block.ops {
                match op {
                    // Direct 6502 branch/call targets lift into Ram space, not Const —
                    // measured in harvest_6502, where a Const-only filter silently
                    // discarded every one.
                    R2ILOp::Call { target }
                        if matches!(target.space, SpaceId::Const | SpaceId::Ram)
                            && in_image(target.offset) =>
                    {
                        g.edge(origin, target.offset, EdgeKind::Call);
                        queue.push_back((target.offset, target.offset));
                    }
                    R2ILOp::CBranch { target, .. }
                        if matches!(target.space, SpaceId::Const | SpaceId::Ram)
                            && in_image(target.offset) =>
                    {
                        // A branch stays inside its own entry: it is control flow, not a
                        // call, so it must not mint a node or an edge.
                        queue.push_back((target.offset, origin));
                    }
                    R2ILOp::Branch { target } => {
                        if matches!(target.space, SpaceId::Const | SpaceId::Ram)
                            && in_image(target.offset)
                        {
                            redirect = Some(target.offset);
                        }
                        stop = true;
                    }
                    R2ILOp::Return { .. } | R2ILOp::BranchInd { .. } | R2ILOp::CallInd { .. } => {
                        stop = true;
                    }
                    _ => {}
                }
            }
            if let Some(t) = redirect {
                addr = t;
                continue;
            }
            if stop {
                break;
            }
            addr += size as u64;
        }
    }
    instructions
}

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

fn corpus_dir() -> PathBuf {
    env::var("C64_CORPUS").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../adaworldapi/elite-source-code-commodore-64/4-reference-binaries/gma86-pal"
        ))
    })
}

/// Where the `.abi` streams land. Sibling of the harvest artifacts, and gitignored for the
/// same reason the ore is: a build product derived from a commercial game's image.
fn out_dir() -> PathBuf {
    env::var("R2IL_MGRA_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../.claude/harvest/r2il-6502"
            ))
        })
}

fn compile_slaspec(input: &PathBuf) -> Result<Vec<u8>, String> {
    let out = env::temp_dir().join(format!("ruff-r2il-mgra-{}", std::process::id()));
    fs::create_dir_all(&out).map_err(|e| format!("scratch: {e}"))?;
    let output = out.join("6502.sla");
    let mut c = SleighCompiler::new(SleighCompilerOptions::default());
    c.compile(input, &output)
        .map_err(|e| format!("sleigh-compiler: {e}"))?;
    fs::read(&output).map_err(|e| format!("read sla: {e}"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec_path = slaspec_path();
    if !spec_path.exists() {
        eprintln!(
            "SKIP: no 6502.slaspec at {} (set C64_SLASPEC)",
            spec_path.display()
        );
        return Ok(());
    }
    let sla = compile_slaspec(&spec_path)?;
    let pspec = fs::read_to_string(spec_path.with_extension("pspec"))?;
    let _spec = build_arch_spec(&sla, &pspec, "6502")?;
    let mut disasm = Disassembler::from_sla(&sla, &pspec, "6502")?;
    disasm.set_userop_map(userop_map_for_arch("6502"));

    let dir = corpus_dir();
    let out = out_dir();
    fs::create_dir_all(&out)?;

    println!("\n=== MGRA v3 emit — the reachability partition as a graph ===");
    println!(
        "{:<20} {:>8} {:>7} {:>7} {:>7} {:>8} {:>9}",
        "file", "instrs", "entries", "refd", "calls", "bytes", "artifact"
    );

    for s in SUBJECTS {
        let Ok(code) = fs::read(dir.join(s.file)) else {
            eprintln!("SKIP {}: not in {}", s.file, dir.display());
            continue;
        };

        let seeds = collect_seeds(&disasm, &code, s.load);
        let mut g = GraphBuilder::new();
        let instructions = walk_into(&disasm, &code, s.load, &seeds, &mut g);

        let (nodes, edges) = g.finish();
        // Counted per class rather than as one total: a single "edges" column cannot show
        // that one of the two arms found nothing, which is exactly the failure a graph of
        // only-calls or only-data-refs would be.
        let entries = nodes
            .iter()
            .filter(|n| n.domain == Palette::Entry.byte())
            .count();
        let referenced = nodes.len() - entries;
        let calls = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Call.byte())
            .count();
        // A refusal here is a defect in this emitter, never a property of the image, so it
        // stops the run loudly rather than writing a stream the renderer would misread.
        let bytes = encode(&nodes, &edges)?;
        let path = out.join(s.out);
        fs::write(&path, &bytes)?;

        println!(
            "{:<20} {:>8} {:>7} {:>7} {:>7} {:>8} {:>9}",
            s.file,
            instructions,
            entries,
            referenced,
            calls,
            bytes.len(),
            s.out
        );
        // Both arms must fire or the graph is half a graph. This warning caught the
        // first version of the walk, whose `Const && in_image` filter could not match
        // anything on these images — a column that read 0 and looked like a measurement.
        if referenced == 0 || calls == 0 {
            eprintln!(
                "  WARNING {}: entries={entries} referenced={referenced} calls={calls} — an \
                 arm of the walk fired zero times, so this graph is one-sided",
                s.file
            );
        }
    }

    println!(
        "\nServe a stream at a URL the field client fetches; `a2ui-graph` parses it directly.\n\
         Node colour is the `domain` byte: {} referenced · {} entry.\n\
         Edge predicate: {} call · {} absolute reference.",
        Palette::Referenced.byte(),
        Palette::Entry.byte(),
        EdgeKind::Call.byte(),
        EdgeKind::AbsRef.byte(),
    );

    Ok(())
}
