//! THE SLAG PARTITION PROBE — is "data lifted as code" separable from "opcode not yet
//! classified"?
//!
//! `harvest_6502` sweeps a whole image as instructions because a 6502 binary has no section
//! headers. This probe asks whether its residual rows split into "opcode not yet classified" vs
//! "data lifted as code".
//!
//! **An earlier run of this probe measured CIPHERTEXT, and every number it produced is void.**
//! Elite's `gma4/5/6` images are ENCRYPTED — the game's own `elite-checksum.asm` ("COMMODORE 64
//! ELITE ENCRYPTION SOURCE"): a running-sum chain `c[i] = p[i] + p[i+1]`, `KEY1=$36` LOCODE /
//! `KEY2=$49` HICODE / `KEY3=$8E` COMLOD. On decrypted plaintext the picture INVERTS: decode-error
//! rises to **87.6%** balanced accuracy while control-flow density FALLS to **60.8%**. The earlier
//! 77% was an artifact of comparing high-entropy ciphertext against structured data; on real code
//! the control-flow densities (p50 22.8 code vs 26.6 data) genuinely cannot separate. A
//! "counterintuitive keeper" recorded at the time — that DATA shows higher control-flow density
//! than code — is false on real code.
//!
//! This probe asks whether the two populations can be told apart, and answers it with a
//! **control group rather than a heuristic**: Elite ships files that are known-pure-data
//! (`SHIPS.bin` ship blueprints, `WORDS.bin` text tokens, `LODATA.bin`, `IANTOK.bin`,
//! `SPRITE.bin`) alongside files that are known-pure-code (`LOCODE.bin`, `HICODE.bin`, which are
//! `gma5.bin`/`gma6.bin` minus their 2-byte PRG header). Both groups go through an IDENTICAL
//! path — same sweep, same furnace, same convention — so any separation in the metrics is a
//! property of the bytes, not of the treatment.
//!
//! **A negative result is a real result here.** If the two groups do not separate, no partition
//! rule is defensible and this probe says so instead of inventing one.
//!
//! **Measured at WINDOW granularity, and that correction is load-bearing.** A first version
//! reported whole-file furnace stats (classified% / residual) and produced two exact duplicate
//! pairs across unrelated files of different sizes — `SPRITE`==`gma5`, `SHIPS`==`gma6`. Cause:
//! it handed every sweep block to ONE `FunctionBehavior`, and `CFG::from_blocks` keeps only the
//! component reachable from `blocks[0]`, so those columns measured a tiny entry-reachable
//! fragment rather than the file. The columns were removed rather than reported. What remains
//! comes straight off the sweep and needs no SSA. The real question is also per-REGION, not
//! per-file: a whole-file aggregate over 8 KB averages code and data together, which is exactly
//! the blur the partition has to see through.
//!
//! **The premise this probe was built to serve turned out to be FALSE, and the probe is what
//! showed it.** It was written believing `harvest_6502` had lifted Elite's ship blueprints and
//! market tables as opcodes, contaminating its residual rows. It had not: Elite keeps data
//! in a SEPARATE source file (`elite-data.asm` → `SHIPS.bin` / `WORDS.bin` / `LODATA.bin` /
//! `IANTOK.bin`), and the harvest corpus was `gma4/5/6` = COMLOD / LOCODE / HICODE, all code.
//! The per-file numbers below corroborate it independently: both code files sit inside the CODE
//! range on both metrics. Some tables ARE embedded in the code image (`QQ23`, the market prices,
//! is in `elite-source.asm`), so contamination is non-zero — but it is not the bulk of the slag,
//! and the earlier reframing that said otherwise was wrong.
//!
//! Corpus is read from a local sibling checkout and never redistributed; a missing file is a
//! printed skip, never a failure.
//!
//! Run:
//! ```sh
//! cargo run --manifest-path crates/ruff_r2il/Cargo.toml --features lift --example slag_partition
//! ```

#![expect(
    clippy::disallowed_methods,
    reason = "not a ty crate: `System` is unavailable to this workspace-excluded crate; these are the probe's env overrides and corpus reads"
)]

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use r2il::{ArchSpec, R2ILBlock};
use r2sleigh_lift::{Disassembler, build_arch_spec, userop_map_for_arch};
use sleigh_compiler::{SleighCompiler, SleighCompilerOptions};

use ruff_r2il::convention::R2ilConvention;
use ruff_r2il::ore::OpTag;

const LIFT_MIN_BYTES: usize = 16;

/// What the corpus asserts about a file, independent of anything measured here. The probe never
/// reads this to decide anything — it is only the answer key the measurements are scored against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Truth {
    Code,
    Data,
}

struct Subject {
    file: &'static str,
    load: u64,
    /// PRG images carry a 2-byte little-endian load address; the raw component files do not.
    prg_header: bool,
    truth: Truth,
}

/// Both groups are drawn from the SAME build of the SAME game, so a separation cannot be an
/// artifact of era, assembler, or compiler.
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

/// Address window size. The partition has to mark REGIONS inside one image, so the unit of
/// measurement is a window, not a file.
const WINDOW: u64 = 256;

#[derive(Default, Debug, Clone)]
struct Metrics {
    bytes: usize,
    /// Offsets where SLEIGH could not decode an instruction at all.
    decode_errors: usize,
    /// Instructions successfully decoded by the linear sweep.
    instructions: usize,
    /// R2IL ops those instructions lowered to.
    /// Control-flow ops (branch / call / return) the sweep saw. Real 6502 code is dense with
    /// these; a data region decoded as code produces them only by coincidence.
    control_flow: usize,
}

impl Metrics {
    fn decode_error_rate(&self) -> f64 {
        let attempts = self.decode_errors + self.instructions;
        if attempts == 0 {
            0.0
        } else {
            self.decode_errors as f64 / attempts as f64
        }
    }
    /// Control-flow ops per 100 decoded instructions — the density that distinguishes a routine
    /// from a table.
    fn cf_density(&self) -> f64 {
        if self.instructions == 0 {
            0.0
        } else {
            100.0 * self.control_flow as f64 / self.instructions as f64
        }
    }
}

fn pad_window(bytes: &[u8], want: usize) -> Vec<u8> {
    let mut w = bytes.to_vec();
    if w.len() < want {
        w.resize(want, 0);
    }
    w
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
    let out_dir = env::temp_dir().join(format!("ruff-r2il-slagpart-{}", std::process::id()));
    fs::create_dir_all(&out_dir).map_err(|e| format!("scratch dir: {e}"))?;
    let output = out_dir.join("6502.sla");
    let mut compiler = SleighCompiler::new(SleighCompilerOptions::default());
    compiler
        .compile(input, &output)
        .map_err(|e| format!("sleigh-compiler: {e}"))?;
    fs::read(&output).map_err(|e| format!("read sla: {e}"))
}

/// One linear sweep, identical for every subject. Fills whole-file metrics AND per-window
/// metrics keyed by window index.
fn sweep(
    disasm: &Disassembler,
    code: &[u8],
    load: u64,
    m: &mut Metrics,
    windows: &mut BTreeMap<u64, Metrics>,
) -> Vec<R2ILBlock> {
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    while offset < code.len() {
        let addr = load + offset as u64;
        let window = pad_window(&code[offset..], LIFT_MIN_BYTES);
        match disasm.lift(&window, addr) {
            Ok(block) => {
                let cf = block.ops.iter().filter(|o| o.is_control_flow()).count();
                m.instructions += 1;
                m.control_flow += cf;
                let w = windows.entry(addr / WINDOW).or_default();
                w.instructions += 1;
                w.control_flow += cf;
                if !block.ops.is_empty() {
                    blocks.push(block.clone());
                }
                offset += (block.size as usize).max(1);
            }
            Err(_) => {
                m.decode_errors += 1;
                windows.entry(addr / WINDOW).or_default().decode_errors += 1;
                offset += 1;
            }
        }
    }
    blocks
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let spec_path = slaspec_path();
    if !spec_path.exists() {
        eprintln!(
            "SKIP: 6502.slaspec not at {} (set C64_SLASPEC)",
            spec_path.display()
        );
        return Ok(());
    }
    let sla = compile_slaspec(&spec_path)?;
    let pspec = fs::read_to_string(spec_path.with_extension("pspec"))?;
    let spec: ArchSpec = build_arch_spec(&sla, &pspec, "6502")?;
    let mut disasm = Disassembler::from_sla(&sla, &pspec, "6502")?;
    disasm.set_userop_map(userop_map_for_arch("6502"));
    let conv = R2ilConvention::from_arch(&spec, pass_one_seven())
        .map_err(|e| format!("convention: {e}"))?;

    let dir = corpus_dir();
    // The convention is built (it validates against the real ArchSpec) but no furnace stats are
    // reported: see the module docs — the whole-file versions were an entry-reachability artifact.
    let _ = &conv;
    let mut all: Vec<(&'static str, Truth, Metrics, Vec<Metrics>)> = Vec::new();

    for s in SUBJECTS {
        let path = dir.join(s.file);
        let Ok(raw) = fs::read(&path) else {
            eprintln!(
                "skip {} — not readable (local sibling corpus, never redistributed)",
                s.file
            );
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

        let mut m = Metrics {
            bytes: code.len(),
            ..Default::default()
        };
        let mut windows: BTreeMap<u64, Metrics> = BTreeMap::new();
        let _blocks = sweep(&disasm, code, load, &mut m, &mut windows);
        // Windows with too few decoded instructions carry no signal; a 3-instruction window can
        // read as 0% or 100% on anything. Dropped rather than allowed to widen a range.
        let ws: Vec<Metrics> = windows
            .into_values()
            .filter(|w| w.instructions >= 16)
            .collect();
        all.push((s.file, s.truth, m, ws));
    }

    if all.is_empty() {
        eprintln!("SKIP: no corpus files readable; nothing measured");
        return Ok(());
    }

    println!("\n=== per-file sweep (whole-file aggregate) ===");
    println!(
        "{:<12} {:>6} {:>8} {:>11} {:>9} {:>8}",
        "file", "truth", "bytes", "decode_err", "cf/100i", "windows"
    );
    for (f, t, m, ws) in &all {
        println!(
            "{:<12} {:>6} {:>8} {:>10.1}% {:>9.1} {:>8}",
            f,
            if *t == Truth::Code { "CODE" } else { "DATA" },
            m.bytes,
            100.0 * m.decode_error_rate(),
            m.cf_density(),
            ws.len()
        );
    }

    // Window-level: pool every scorable window per group.
    let pool = |t: Truth, f: &dyn Fn(&Metrics) -> f64| -> Vec<f64> {
        let mut v: Vec<f64> = all
            .iter()
            .filter(|(_, tt, _, _)| *tt == t)
            .flat_map(|(_, _, _, ws)| ws.iter().map(f))
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    };
    let pct = |v: &[f64], p: f64| -> f64 {
        if v.is_empty() {
            return f64::NAN;
        }
        v[(((v.len() - 1) as f64) * p).round() as usize]
    };

    println!("\n=== window-level distributions ({WINDOW}-byte windows, >=16 instructions) ===");
    for (label, f) in [
        (
            "decode error %",
            &(|m: &Metrics| 100.0 * m.decode_error_rate()) as &dyn Fn(&Metrics) -> f64,
        ),
        (
            "cf per 100 instr",
            &(|m: &Metrics| m.cf_density()) as &dyn Fn(&Metrics) -> f64,
        ),
    ] {
        let c = pool(Truth::Code, f);
        let d = pool(Truth::Data, f);
        println!(
            "{label:<18} CODE n={:<4} p10={:6.1} p50={:6.1} p90={:6.1}",
            c.len(),
            pct(&c, 0.1),
            pct(&c, 0.5),
            pct(&c, 0.9)
        );
        println!(
            "{:<18} DATA n={:<4} p10={:6.1} p50={:6.1} p90={:6.1}",
            "",
            d.len(),
            pct(&d, 0.1),
            pct(&d, 0.5),
            pct(&d, 0.9)
        );
        // Best single threshold, scored by balanced accuracy. Reported even when poor, so a weak
        // discriminator cannot be mistaken for a good one.
        let mut best = (0.0f64, 0.0f64, false);
        let mut cands: Vec<f64> = c.iter().chain(d.iter()).cloned().collect();
        cands.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for &threshold in &cands {
            for hi_is_data in [true, false] {
                let dt = d
                    .iter()
                    .filter(|&&x| {
                        if hi_is_data {
                            x >= threshold
                        } else {
                            x < threshold
                        }
                    })
                    .count() as f64
                    / d.len().max(1) as f64;
                let ct = c
                    .iter()
                    .filter(|&&x| {
                        if hi_is_data {
                            x < threshold
                        } else {
                            x >= threshold
                        }
                    })
                    .count() as f64
                    / c.len().max(1) as f64;
                let bal = (dt + ct) / 2.0;
                if bal > best.1 {
                    best = (threshold, bal, hi_is_data);
                }
            }
        }
        println!(
            "{:<18} best split: {} {:.1}  =>  balanced accuracy {:.1}%  ({})",
            "",
            if best.2 { "data >=" } else { "data <" },
            best.0,
            100.0 * best.1,
            if best.1 >= 0.90 {
                "USABLE"
            } else if best.1 >= 0.75 {
                "weak — not alone"
            } else {
                "no better than guessing"
            }
        );
    }

    Ok(())
}
