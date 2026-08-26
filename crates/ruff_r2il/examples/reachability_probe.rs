//! THE REACHABILITY PROBE — the hypothesis `slag_partition` left standing.
//!
//! `slag_partition` showed that CONTENT statistics do not separate code from data on a 6502
//! image: on DECRYPTED plaintext, decode-error rate reaches 87.6% balanced accuracy and
//! control-flow density only 60.8%. Neither is usable alone. What it could not test — standalone
//! control files have no callers — is the hypothesis that follows from the substrate's doctrine:
//!
//! > **Code is what a call graph REACHES. Data is what is only ever loaded FROM.**
//!
//! A byte's role is a property of its ADDRESS's position in the graph, not of the byte's value.
//!
//! **Correction, 2026-08-26: the `unreached+ref` column was structurally zero until now.** It
//! filtered `addr.space == Const && in_image(addr)`, which cannot match on this architecture —
//! see [`Walk::data_refs`]. With the reference resolved properly through the block
//! ([`ruff_r2il::absref`]) the column is a real measurement, and it says something: of 60
//! in-image referenced addresses across both code images, **58 are never reached as code**
//! (LOCODE 7 of 9, HICODE 51 of 51). That is the "data is what is only ever loaded from" half
//! of the hypothesis, which the reached% column alone cannot show.
//! So this probe replaces the linear sweep with **recursive descent** from the entry plus every
//! discovered call target, and asks whether the reached set discriminates.
//!
//! ## What is a lens and what is evidence (this distinction is load-bearing)
//!
//! Everything here is **cast-reproducible**: the same image always yields the same reached set.
//! By the zero-copy law that makes reachability a **LENS** — recomputed or masked, never stored.
//! A persisted code/data map would be exactly the second truth this whole arc exists to remove.
//! A future dynamic arm (observing real `MemRead`/`MemWrite` during execution) is a different
//! rung: it is NOT derivable from the image, two runs disagree, and it may legitimately be
//! stored as evidence. This probe deliberately contains only the derivable half.
//!
//! ## The measured edge case, and why it is the best falsifier
//!
//! Elite has **zero `JMP ($xxxx)`** — the worst enemy of static analysis is absent. But it does
//! have jump tables (`JMTB`, plus two music-command tables), dispatched by index. Recursive
//! descent cannot follow those, so table-only-reachable code lands in the unreached set and looks
//! like data. That is not a flaw to route around: a jump table is genuinely a data table whose
//! VALUES are code entry points — one region, two legitimate readings — and it is the case a
//! ClassView has to express without a `this_is_a_jump_table` flag.
//!
//! ## KILL CONDITION, pre-registered
//!
//! If the code images come back **>= 95% reached**, reachability cannot discriminate either (it
//! marks nearly everything), and this probe reports that instead of proposing a rule. Likewise if
//! the data controls do not sit clearly below the code images.
//!
//! ## Seeding — the load address is NOT the entry point (measured)
//!
//! A first version seeded the descent at the load address and reported **0.0% reached, 1
//! instruction** on every subject. That was a broken probe, not a result: `gma5` begins with
//! `$00` = BRK, which lifts to an immediate stop, and the others halt within one instruction.
//! These are raw code blobs loaded at an address and entered from elsewhere — self-starting PRGs
//! they are not. So seeds come from a LINEAR SWEEP that collects every in-image call target
//! (`harvest_6502`'s JSR partitioning, reused), and recursive descent runs from all of them.
//! Linear sweep finds candidate entries; descent computes reachability. Data controls get the
//! identical treatment, spurious seeds included, so the comparison stays fair.
//!
//! Corpus is a local sibling checkout, never redistributed; a missing file is a printed skip.
//!
//! Run:
//! ```sh
//! cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example reachability_probe
//! ```

#![expect(
    clippy::disallowed_methods,
    reason = "not a ty crate: `System` is unavailable to this workspace-excluded crate; these are the probe's env overrides and corpus reads"
)]

use std::collections::{BTreeSet, VecDeque};
use std::env;
use std::fs;
use std::path::PathBuf;

use r2il::{R2ILOp, SpaceId};
use r2sleigh_lift::{Disassembler, build_arch_spec, userop_map_for_arch};
use ruff_r2il::absref::absolute_refs;
use sleigh_compiler::{SleighCompiler, SleighCompilerOptions};

const LIFT_MIN_BYTES: usize = 16;

/// Pre-registered kill threshold: at or above this, "reached" marks nearly everything and the
/// signal is vacuous.
const KILL_REACHED_PCT: f64 = 95.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Truth {
    Code,
    Data,
}

struct Subject {
    file: &'static str,
    load: u64,
    prg_header: bool,
    truth: Truth,
}

const SUBJECTS: &[Subject] = &[
    // DECRYPTED plaintext 6502. The gma4/5/6 images are ENCRYPTED (Elite's own
    // elite-checksum.asm, "COMMODORE 64 ELITE ENCRYPTION SOURCE": a running-sum chain,
    // KEY1=$36 / KEY2=$49 / KEY3=$8E, with trailing unencrypted padding). Measuring on
    // those measured ciphertext; every earlier number from them is void.
    Subject {
        file: "LOCODE.unprot.bin",
        load: 0x1d00,
        prg_header: false,
        truth: Truth::Code,
    },
    Subject {
        file: "HICODE.unprot.bin",
        load: 0x6a00,
        prg_header: false,
        truth: Truth::Code,
    },
    Subject {
        file: "SHIPS.bin",
        load: 0x1000,
        prg_header: false,
        truth: Truth::Data,
    },
    Subject {
        file: "WORDS.bin",
        load: 0x1000,
        prg_header: false,
        truth: Truth::Data,
    },
    Subject {
        file: "LODATA.bin",
        load: 0x1000,
        prg_header: false,
        truth: Truth::Data,
    },
    Subject {
        file: "IANTOK.bin",
        load: 0x1000,
        prg_header: false,
        truth: Truth::Data,
    },
    Subject {
        file: "SPRITE.bin",
        load: 0x1000,
        prg_header: false,
        truth: Truth::Data,
    },
];

struct Walk {
    /// Bytes covered by an instruction recursive descent actually decoded.
    reached: BTreeSet<u64>,
    /// In-image addresses named by a resolved absolute reference — "referenced AS DATA". A byte
    /// both unreached and data-referenced is the strongest data evidence available without
    /// running the program.
    ///
    /// Resolved by [`ruff_r2il::absref`], not read off the address operand. The earlier
    /// `addr.space == Const && in_image(addr)` filter made this set STRUCTURALLY EMPTY: a full
    /// linear sweep of LOCODE finds 1418 load/store address operands — 860 `Unique`, 328
    /// `Register`, 230 `Const` — and not one `Const` operand inside the image, because 6502
    /// absolute-indexed addressing puts the base in the constant operand of the `IntAdd` that
    /// defines the address temp. The `unreached+ref` column read 0.00% and looked like a
    /// measurement.
    data_refs: BTreeSet<u64>,
    /// Call targets discovered during the walk (the 6502 stand-in for a symbol table).
    calls: BTreeSet<u64>,
    instructions: usize,
}

fn pad(bytes: &[u8], want: usize) -> Vec<u8> {
    let mut w = bytes.to_vec();
    if w.len() < want {
        w.resize(want, 0);
    }
    w
}

/// Linear sweep collecting every in-image call target — the seed set. A symbol table would
/// supply these; a PRG has none.
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

/// Recursive descent from `entry`. Follows fall-through, both arms of a conditional branch, the
/// target of an unconditional branch, and enqueues call targets while continuing after the call.
/// Stops a path at a return, an indirect transfer (`BranchInd`/`CallInd` — honestly unfollowable
/// without a value analysis), or a decode failure.
fn walk(disasm: &Disassembler, code: &[u8], load: u64, entries: &BTreeSet<u64>) -> Walk {
    let end = load + code.len() as u64;
    let mut w = Walk {
        reached: BTreeSet::new(),
        data_refs: BTreeSet::new(),
        calls: BTreeSet::new(),
        instructions: 0,
    };
    let mut queue: VecDeque<u64> = VecDeque::new();
    let mut seen: BTreeSet<u64> = BTreeSet::new();
    for &e in entries {
        queue.push_back(e);
    }

    let in_image = |a: u64| a >= load && a < end;

    while let Some(start) = queue.pop_front() {
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
            w.instructions += 1;
            for b in 0..size as u64 {
                w.reached.insert(addr + b);
            }

            // Static data references, resolved through the block rather than read off the
            // address operand. See `ruff_r2il::absref`: on 6502 the address of an
            // absolute-indexed access is the constant operand of the `IntAdd` that defines
            // the address temp, so the old `addr.space == Const` filter could never match
            // and this set was structurally empty.
            for a in absolute_refs(&block.ops) {
                if in_image(a) {
                    w.data_refs.insert(a);
                }
            }

            let mut stop = false;
            let mut redirect: Option<u64> = None;
            for op in &block.ops {
                match op {
                    // Direct 6502 branch/call targets lift into Ram space, not Const — measured
                    // in harvest_6502, where a Const-only filter silently discarded every one.
                    R2ILOp::Call { target }
                        if matches!(target.space, SpaceId::Const | SpaceId::Ram)
                            && in_image(target.offset) =>
                    {
                        w.calls.insert(target.offset);
                        queue.push_back(target.offset);
                    }
                    R2ILOp::CBranch { target, .. }
                        if matches!(target.space, SpaceId::Const | SpaceId::Ram)
                            && in_image(target.offset) =>
                    {
                        queue.push_back(target.offset);
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
    w
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

fn compile_slaspec(input: &PathBuf) -> Result<Vec<u8>, String> {
    let out_dir = env::temp_dir().join(format!("ruff-r2il-reach-{}", std::process::id()));
    fs::create_dir_all(&out_dir).map_err(|e| format!("scratch: {e}"))?;
    let output = out_dir.join("6502.sla");
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
    let spec = build_arch_spec(&sla, &pspec, "6502")?;
    let mut disasm = Disassembler::from_sla(&sla, &pspec, "6502")?;
    disasm.set_userop_map(userop_map_for_arch("6502"));
    let _ = &spec;

    let dir = corpus_dir();
    let mut rows: Vec<(&'static str, Truth, f64, f64, usize, usize)> = Vec::new();

    println!("\n=== recursive descent from entry + discovered call targets ===");
    println!(
        "{:<12} {:>6} {:>8} {:>10} {:>7} {:>12} {:>8} {:>10}",
        "file", "truth", "bytes", "reached%", "refs", "unreached+ref", "seeds", "instrs"
    );

    for s in SUBJECTS {
        let Ok(raw) = fs::read(dir.join(s.file)) else {
            eprintln!("skip {} — not readable (local sibling corpus)", s.file);
            continue;
        };
        let (load, code): (u64, &[u8]) = if s.prg_header {
            if raw.len() < 3 {
                continue;
            }
            (u64::from(u16::from_le_bytes([raw[0], raw[1]])), &raw[2..])
        } else {
            (s.load, &raw[..])
        };
        let seeds = collect_seeds(&disasm, code, load);
        let w = walk(&disasm, code, load, &seeds);
        let total = code.len() as f64;
        let reached_pct = 100.0 * w.reached.len() as f64 / total;
        // Bytes never reached as code AND named by a resolved absolute reference: the
        // strongest static evidence of a data role.
        //
        // Reported next to the RAW in-image reference count on purpose. One number cannot
        // distinguish "the resolver found nothing" from "it found plenty and they were all
        // reached code" — and the first of those is what this column silently was before
        // `absref` landed.
        let unreached_ref = w
            .data_refs
            .iter()
            .filter(|a| !w.reached.contains(a))
            .count();
        let unreached_ref_pct = 100.0 * unreached_ref as f64 / total;
        println!(
            "{:<12} {:>6} {:>8} {:>9.1}% {:>7} {:>11.2}% {:>8} {:>10}",
            s.file,
            if s.truth == Truth::Code {
                "CODE"
            } else {
                "DATA"
            },
            code.len(),
            reached_pct,
            w.data_refs.len(),
            unreached_ref_pct,
            seeds.len(),
            w.instructions
        );
        rows.push((
            s.file,
            s.truth,
            reached_pct,
            unreached_ref_pct,
            seeds.len(),
            w.instructions,
        ));
    }

    if rows.is_empty() {
        eprintln!("SKIP: no corpus readable; nothing measured");
        return Ok(());
    }

    let rng = |t: Truth, i: usize| -> (f64, f64) {
        let v: Vec<f64> = rows
            .iter()
            .filter(|r| r.1 == t)
            .map(|r| if i == 0 { r.2 } else { r.3 })
            .collect();
        (
            v.iter().cloned().fold(f64::MAX, f64::min),
            v.iter().cloned().fold(f64::MIN, f64::max),
        )
    };

    println!("\n=== verdict ===");
    let (c0, c1) = rng(Truth::Code, 0);
    let (d0, d1) = rng(Truth::Data, 0);
    println!("reached%   CODE [{c0:.1} .. {c1:.1}]   DATA [{d0:.1} .. {d1:.1}]");

    if c0 >= KILL_REACHED_PCT {
        println!("KILLED: code images are >= {KILL_REACHED_PCT:.0}% reached — 'reached' marks");
        println!("        nearly everything, so it discriminates nothing. No rule proposed.");
    } else if d1 < c0 {
        println!("SEPARATES: every data control is below every code image, ranges disjoint.");
        println!("        Reachability is a usable role signal where content statistics were not");
        println!("        (slag_partition on the same plaintext: 87.6% / 60.8%).");
    } else {
        println!("OVERLAPS: ranges intersect — reachability alone is not a discriminator either.");
        println!("        Reported as measured; no threshold invented to paper over it.");
    }
    println!("\nNOTE the unreached set on a CODE image is NOT all data: Elite dispatches through");
    println!("jump tables (JMTB + two music tables), which recursive descent cannot follow, so");
    println!("table-only-reachable code lands there too. That is the ClassView case — a table");
    println!("whose VALUES are code entry points — not a defect to threshold away.");
    Ok(())
}
