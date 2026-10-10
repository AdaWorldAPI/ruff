//! Stage 5 · the SINK — where refined truth lands.
//!
//! `crate::furnace::smelt` already returns `Vec<FlatFact>` + `ResidualLedger` +
//! `HarvestReport` in memory and persists nothing (`furnace`/`ore`/`slag` carry
//! **no** persistence assumption — verified: neither module contains a file,
//! network, or env-var read). This module is PR 2's addition: a trait behind
//! which where-it-lands is chosen at the call site, never baked into the
//! furnace.
//!
//! # SUBSTRATE RULING (operator, 2026-08-18), restated here for the reader who
//! lands on this file without the plan open
//!
//! Three backends, **none privileged**:
//!
//! 1. **Offline V3 substrate** — file/artifact-shaped, self-contained, no
//!    service. [`OfflineSink`] below. What the PR-1 harvest examples already
//!    write by hand; this gives that shape a trait and a reader.
//! 2. **lance-graph zero-copy SoA** — live storage AND audit layer. **Not
//!    implemented in this crate.** `ruff_r2il` does not and should not depend
//!    on `lance-graph` (see `facet.rs`'s own PR-3 deferral: the real classid
//!    mint is a canon-high slot lance-graph owns, not this crate). A
//!    lance-graph-side crate implements [`RefinedTruthSink`] against its own
//!    SoA — this trait is the seam, not a lance-graph binding.
//! 3. **S3 object storage via `AWS_*` env vars (Railway Tigris)** — credential
//!    plumbing, not a fourth format. [`S3Config::from_env`] below reads the
//!    environment and resolves `None` when unset, matching the ruling's own
//!    words: *"if unset fall back to local offline mode rather than
//!    failing."* **Honesty note: the actual PUT is NOT implemented here.**
//!    Wiring a signed S3 upload is real scope (SigV4 or an SDK dependency)
//!    that deserves its own measured pass rather than being shipped untested
//!    alongside everything else in this file. `S3Sink` is deliberately absent
//!    — do not fake one that silently no-ops on `write_harvest`.
//!
//! # The offline artifact format
//!
//! Same discipline `.claude/harvest/r2il/STAGED-CODEGEN-GUIDE.md` §4 already
//! states for every R2IL artifact: `#version N` + `#schema <names>` header,
//! columns appended never reordered, a reader keys off `#schema` names, never
//! column position. [`OfflineSink`] writes three files per harvest — facts,
//! residuals, and the report — under one directory, so a consumer can read
//! whichever it needs without parsing the others.
//!
//! # v1 → v2 (this pass): residuals gained the payload the reader needs
//!
//! v1's residuals writer recorded `reason.as_str()` + the facet coordinate —
//! honest about being *addressed*, but silently lossy for anything a reader
//! would need to RECONSTRUCT a [`ResidualFact`]: the per-variant payload
//! (e.g. which [`OpTag`] was unclassified), [`ResidualFact::at_prefix`], and
//! [`ResidualFact::provenance`] were never written at all. v1 was never
//! shipped with a reader, so this widens the schema (bumping
//! `RESIDUALS_VERSION`) rather than living with the gap — additive per the
//! rule above: every v1 column keeps its meaning, new columns append.
//! [`read_facts`] and [`read_residuals`] close the "honesty note" the
//! previous pass of this file left on [`OfflineSink::new`].

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use r2ssa::{BlockId, InstId, ValueId};

use crate::facet::{FacetPrefix, VarnodeFacet};
use crate::furnace::{Concern, FactId, FactKind, FlatFact, HarvestReport};
use crate::ore::{FactProvenance, OpTag};
use crate::slag::{ResidualFact, ResidualLedger, ResidualReason};

/// Where refined truth (one `smelt` call's output) lands. Implemented by
/// [`OfflineSink`] here; a lance-graph-side crate implements it for its own
/// SoA (see the module docs' SUBSTRATE RULING above — this crate ships no
/// such implementation and takes no lance-graph dependency to do so).
pub trait RefinedTruthSink {
    type Error;

    /// Persist one harvest. `source` is a caller-supplied label (e.g. the
    /// binary path or corpus identity) — never derived from the facts
    /// themselves, since [`FlatFact`] carries no notion of "which binary".
    fn write_harvest(
        &mut self,
        source: &str,
        facts: &[FlatFact],
        residuals: &ResidualLedger,
        report: &HarvestReport,
    ) -> Result<(), Self::Error>;
}

/// Backend 1 — file/artifact-shaped, self-contained, no service. Writes
/// `<dir>/<source>.facts.tsv`, `<dir>/<source>.residuals.tsv`,
/// `<dir>/<source>.report.tsv`. `source` is sanitised for the filesystem
/// (see [`sanitise_source`]) so an arbitrary path-shaped label never escapes
/// `dir`.
#[derive(Debug, Clone)]
pub struct OfflineSink {
    dir: PathBuf,
}

impl OfflineSink {
    /// Does NOT create `dir` — call [`Self::ensure_dir`] first, or write into
    /// a directory the caller already knows exists. Kept a separate step so
    /// a sink can be constructed purely to compute the `dir`/`source` join
    /// the [`read_report`]/[`read_facts`]/[`read_residuals`] readers need,
    /// without a filesystem side effect.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// # Errors
    /// Propagates [`std::fs::create_dir_all`]'s error.
    #[expect(
        clippy::disallowed_methods,
        reason = "ruff/clippy.toml's disallowed-methods list is directory-scoped, not \
            Cargo-workspace-scoped, so it reaches this workspace-EXCLUDED crate even \
            though its reasons say 'in ty crates' — ruff_r2il has no ty::System trait \
            to route through; plain std::fs is correct here"
    )]
    pub fn ensure_dir(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.dir)
    }

    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path_for(&self, source: &str, suffix: &str) -> PathBuf {
        self.dir
            .join(format!("{}.{suffix}", sanitise_source(source)))
    }
}

/// Filesystem-safe form of a source label: every byte outside
/// `[A-Za-z0-9._-]` becomes `_`. Two DIFFERENT sources that sanitise to the
/// SAME string would collide — that is a caller error (pick disjoint
/// labels), not something this function silently disambiguates, since
/// disambiguating would make the on-disk name depend on write ORDER.
#[must_use]
pub fn sanitise_source(source: &str) -> String {
    source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Everything [`OfflineSink::write_harvest`] can fail on.
#[derive(Debug)]
pub enum OfflineSinkError {
    Io(io::Error),
}

impl fmt::Display for OfflineSinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OfflineSinkError::Io(e) => write!(f, "offline sink I/O error: {e}"),
        }
    }
}

impl std::error::Error for OfflineSinkError {}

impl From<io::Error> for OfflineSinkError {
    fn from(e: io::Error) -> Self {
        OfflineSinkError::Io(e)
    }
}

const FACTS_VERSION: u32 = 2;
const FACTS_SCHEMA: &str = "id\tspace_discriminant\toffset\tsize\tconcern\tkind\topcode\ta\tb\tblock_addr\top_idx\tinst_id\tvalue_id";

const RESIDUALS_VERSION: u32 = 2;
const RESIDUALS_SCHEMA: &str = "reason\treason_payload1\treason_payload2\tspace_discriminant\toffset\tsize\tprefix_kind\tprefix_discriminant\tprefix_offset\tprefix_size\tblock_addr\top_idx\tinst_id\tvalue_id";

const REPORT_VERSION: u32 = 1;
const REPORT_SCHEMA: &str = "harvested\tclassified\tresidual\tdropped";

impl RefinedTruthSink for OfflineSink {
    type Error = OfflineSinkError;

    fn write_harvest(
        &mut self,
        source: &str,
        facts: &[FlatFact],
        residuals: &ResidualLedger,
        report: &HarvestReport,
    ) -> Result<(), Self::Error> {
        self.ensure_dir()?;

        let mut w = BufWriter::new(File::create(self.path_for(source, "facts.tsv"))?);
        writeln!(w, "#version {FACTS_VERSION}")?;
        writeln!(w, "#schema {FACTS_SCHEMA}")?;
        for fact in facts {
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                fact.id.0,
                fact.at.space_discriminant(),
                fact.at.offset(),
                fact.at.size(),
                concern_str(fact.concern),
                fact.kind.as_str(),
                fact.opcode.as_str(),
                fact.a,
                fact.b,
                provenance_cols(&fact.prov),
            )?;
        }
        w.flush()?;

        let mut w = BufWriter::new(File::create(self.path_for(source, "residuals.tsv"))?);
        writeln!(w, "#version {RESIDUALS_VERSION}")?;
        writeln!(w, "#schema {RESIDUALS_SCHEMA}")?;
        for row in residuals.rows() {
            let (p1, p2) = reason_payload_cols(&row.reason);
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}",
                row.reason.as_str(),
                p1,
                p2,
                facet_cols(row.at),
                prefix_cols(row.at_prefix),
                provenance_cols(&row.provenance),
            )?;
        }
        w.flush()?;

        let mut w = BufWriter::new(File::create(self.path_for(source, "report.tsv"))?);
        writeln!(w, "#version {REPORT_VERSION}")?;
        writeln!(w, "#schema {REPORT_SCHEMA}")?;
        writeln!(
            w,
            "{}\t{}\t{}\t{}",
            report.harvested, report.classified, report.residual, report.dropped
        )?;
        w.flush()?;

        Ok(())
    }
}

// ================================================================================================
// Shared column codecs — used by both the facts and residuals writers/readers
// ================================================================================================

fn concern_str(c: Concern) -> &'static str {
    match c {
        Concern::Control => "control",
        Concern::Values => "values",
        Concern::Objects => "objects",
        Concern::Memory => "memory",
        Concern::Predicates => "predicates",
        Concern::Calls => "calls",
    }
}

fn concern_from_str(s: &str) -> Option<Concern> {
    Some(match s {
        "control" => Concern::Control,
        "values" => Concern::Values,
        "objects" => Concern::Objects,
        "memory" => Concern::Memory,
        "predicates" => Concern::Predicates,
        "calls" => Concern::Calls,
        _ => return None,
    })
}

fn fact_kind_from_str(s: &str) -> Option<FactKind> {
    Some(match s {
        "op" => FactKind::Op,
        "operand_in" => FactKind::OperandIn,
        "operand_out" => FactKind::OperandOut,
        "edge" => FactKind::Edge,
        "mem_use" => FactKind::MemUse,
        "mem_def" => FactKind::MemDef,
        "predicate" => FactKind::Predicate,
        "call_site" => FactKind::CallSite,
        _ => return None,
    })
}

/// `block_addr\top_idx\tinst_id\tvalue_id` — four columns, one call site per
/// [`FactProvenance`] anywhere it appears in either TSV. Kept as ONE function
/// so the facts and residuals writers can never drift apart on this shape.
fn provenance_cols(prov: &FactProvenance) -> String {
    format!(
        "{}\t{}\t{}\t{}",
        prov.block.map_or_else(String::new, |b| b.0.to_string()),
        prov.op_site
            .map_or_else(String::new, |(addr, idx)| format!("{addr}:{idx}")),
        prov.inst.map_or_else(String::new, |i| i.0.to_string()),
        prov.value.map_or_else(String::new, |v| v.0.to_string()),
    )
}

fn parse_provenance_cols<'a>(
    cols: &mut impl Iterator<Item = &'a str>,
) -> io::Result<FactProvenance> {
    let block = next_opt(cols)?
        .map(|s| s.parse().map(BlockId))
        .transpose()
        .map_err(|_| malformed("non-numeric block_addr"))?;
    let op_site = next_opt(cols)?
        .map(|s| {
            let (addr, idx) = s
                .split_once(':')
                .ok_or_else(|| malformed("op_site missing ':' separator"))?;
            let addr: u64 = addr
                .parse()
                .map_err(|_| malformed("non-numeric op_site addr"))?;
            let idx: usize = idx
                .parse()
                .map_err(|_| malformed("non-numeric op_site idx"))?;
            Ok::<(u64, usize), io::Error>((addr, idx))
        })
        .transpose()?;
    let inst = next_opt(cols)?
        .map(|s| s.parse().map(InstId))
        .transpose()
        .map_err(|_| malformed("non-numeric inst_id"))?;
    let value = next_opt(cols)?
        .map(|s| s.parse().map(ValueId))
        .transpose()
        .map_err(|_| malformed("non-numeric value_id"))?;
    Ok(FactProvenance {
        inst,
        block,
        op_site,
        value,
    })
}

/// `space_discriminant\toffset\tsize` for an `Option<VarnodeFacet>` — empty
/// columns when `None` (the [`ResidualReason::NoFacetCoordinate`] case).
fn facet_cols(at: Option<VarnodeFacet>) -> String {
    format!(
        "{}\t{}\t{}",
        at.map(|f| f.space_discriminant())
            .map_or_else(String::new, |d| d.to_string()),
        at.map(|f| f.offset())
            .map_or_else(String::new, |o| o.to_string()),
        at.map(|f| f.size())
            .map_or_else(String::new, |s| s.to_string()),
    )
}

fn parse_facet_cols<'a>(
    cols: &mut impl Iterator<Item = &'a str>,
) -> io::Result<Option<VarnodeFacet>> {
    let discriminant = next_opt(cols)?
        .map(str::parse::<u16>)
        .transpose()
        .map_err(|_| malformed("non-numeric space_discriminant"))?;
    let offset = next_opt(cols)?
        .map(str::parse::<u64>)
        .transpose()
        .map_err(|_| malformed("non-numeric offset"))?;
    let size = next_opt(cols)?
        .map(str::parse::<u32>)
        .transpose()
        .map_err(|_| malformed("non-numeric size"))?;
    Ok(match (discriminant, offset, size) {
        (Some(d), Some(o), Some(s)) => Some(facet_from_raw(d, o, s)),
        (None, None, None) => None,
        _ => return Err(malformed("facet columns partially present")),
    })
}

/// Builds a [`VarnodeFacet`] directly from its three logical fields, using
/// the EXACT byte layout `facet.rs` documents on the type itself (all
/// little-endian: `0..4` classid = `(PROVISIONAL_R2IL_VARNODE << 16) |
/// discriminant`, `4..8` offset lo, `8..12` offset hi, `12..16` size).
/// [`VarnodeFacet`]'s only other constructors (`facet::project`/`unproject`)
/// go through a real [`r2il::Varnode`] + [`crate::facet::CustomSpaceTable`],
/// which a TSV reader does not have — this is the read-back-shaped inverse
/// of [`VarnodeFacet::space_discriminant`]/`offset`/`size`, and belongs here
/// rather than in `facet.rs` because it is a PERSISTENCE-layer concern (PR
/// 2's own "Role 2" per that module's doc comment), not an address-scheme
/// one.
#[must_use]
fn facet_from_raw(discriminant: u16, offset: u64, size: u32) -> VarnodeFacet {
    let classid: u32 =
        (u32::from(crate::facet::PROVISIONAL_R2IL_VARNODE) << 16) | u32::from(discriminant);
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&classid.to_le_bytes());
    bytes[4..8].copy_from_slice(&(offset as u32).to_le_bytes());
    bytes[8..12].copy_from_slice(&((offset >> 32) as u32).to_le_bytes());
    bytes[12..16].copy_from_slice(&size.to_le_bytes());
    VarnodeFacet(bytes)
}

/// `prefix_kind\tprefix_discriminant\tprefix_offset\tprefix_size` for an
/// `Option<FacetPrefix>`. `prefix_kind` is `space`/`space_offset`/
/// `space_offset_size`, empty for `None`; the other three columns are
/// populated only as far as that variant carries them (e.g. `Space` leaves
/// `prefix_offset`/`prefix_size` empty).
fn prefix_cols(p: Option<FacetPrefix>) -> String {
    match p {
        None => "\t\t\t".to_string(),
        Some(FacetPrefix::Space { discriminant }) => format!("space\t{discriminant}\t\t"),
        Some(FacetPrefix::SpaceOffset {
            discriminant,
            offset,
        }) => {
            format!("space_offset\t{discriminant}\t{offset}\t")
        }
        Some(FacetPrefix::SpaceOffsetSize {
            discriminant,
            offset,
            size,
        }) => {
            format!("space_offset_size\t{discriminant}\t{offset}\t{size}")
        }
    }
}

fn parse_prefix_cols<'a>(
    cols: &mut impl Iterator<Item = &'a str>,
) -> io::Result<Option<FacetPrefix>> {
    let kind = next_opt(cols)?;
    let discriminant = next_opt(cols)?;
    let offset = next_opt(cols)?;
    let size = next_opt(cols)?;
    Ok(match kind {
        None => None,
        Some("space") => {
            let discriminant = discriminant
                .ok_or_else(|| malformed("prefix missing discriminant"))?
                .parse()
                .map_err(|_| malformed("non-numeric prefix discriminant"))?;
            Some(FacetPrefix::Space { discriminant })
        }
        Some("space_offset") => {
            let discriminant = discriminant
                .ok_or_else(|| malformed("prefix missing discriminant"))?
                .parse()
                .map_err(|_| malformed("non-numeric prefix discriminant"))?;
            let offset = offset
                .ok_or_else(|| malformed("prefix missing offset"))?
                .parse()
                .map_err(|_| malformed("non-numeric prefix offset"))?;
            Some(FacetPrefix::SpaceOffset {
                discriminant,
                offset,
            })
        }
        Some("space_offset_size") => {
            let discriminant = discriminant
                .ok_or_else(|| malformed("prefix missing discriminant"))?
                .parse()
                .map_err(|_| malformed("non-numeric prefix discriminant"))?;
            let offset = offset
                .ok_or_else(|| malformed("prefix missing offset"))?
                .parse()
                .map_err(|_| malformed("non-numeric prefix offset"))?;
            let size = size
                .ok_or_else(|| malformed("prefix missing size"))?
                .parse()
                .map_err(|_| malformed("non-numeric prefix size"))?;
            Some(FacetPrefix::SpaceOffsetSize {
                discriminant,
                offset,
                size,
            })
        }
        Some(other) => return Err(malformed_owned(format!("unknown prefix_kind {other:?}"))),
    })
}

/// The two generic payload columns for one [`ResidualReason`], covering
/// every variant's own payload shape (never a lossy summary):
/// `OpTag`-carrying variants write the tag's `as_str()`; numeric variants
/// write the number as text; the two-numeric variant
/// ([`ResidualReason::PhiFanInExceedsPredecessors`]) uses both columns; the
/// two-`OpTag` variant ([`ResidualReason::OpSiteJoinMismatch`]) uses both
/// columns; payload-free variants write two empty columns.
fn reason_payload_cols(r: &ResidualReason) -> (String, String) {
    match r {
        ResidualReason::OpcodeNotInConvention { opcode } => {
            (opcode.as_str().to_string(), String::new())
        }
        ResidualReason::UserOpNotInConvention { userop } => (userop.to_string(), String::new()),
        ResidualReason::CustomSpaceNotInConvention { raw } => (raw.to_string(), String::new()),
        ResidualReason::FacetOverflowAtKey { raw } => (raw.to_string(), String::new()),
        ResidualReason::VariadicArity { arity } => (arity.to_string(), String::new()),
        ResidualReason::PhiFanInExceedsPredecessors {
            inputs,
            predecessors,
        } => (inputs.to_string(), predecessors.to_string()),
        ResidualReason::OpSiteJoinMismatch { expected, found } => {
            (expected.as_str().to_string(), found.as_str().to_string())
        }
        ResidualReason::NoConventionRowAtAddress
        | ResidualReason::MemoryObjectEscaped
        | ResidualReason::IndirectTarget
        | ResidualReason::NoFacetCoordinate => (String::new(), String::new()),
    }
}

fn parse_reason(name: &str, p1: &str, p2: &str) -> io::Result<ResidualReason> {
    let parse_opcode = |s: &str| -> io::Result<OpTag> {
        OpTag::parse(s).ok_or_else(|| malformed_owned(format!("unknown opcode {s:?}")))
    };
    let parse_u32 = |s: &str| -> io::Result<u32> {
        s.parse()
            .map_err(|_| malformed("non-numeric reason payload"))
    };
    let parse_usize = |s: &str| -> io::Result<usize> {
        s.parse()
            .map_err(|_| malformed("non-numeric reason payload"))
    };
    Ok(match name {
        "opcode_not_in_convention" => ResidualReason::OpcodeNotInConvention {
            opcode: parse_opcode(p1)?,
        },
        "no_convention_row_at_address" => ResidualReason::NoConventionRowAtAddress,
        "userop_not_in_convention" => ResidualReason::UserOpNotInConvention {
            userop: parse_u32(p1)?,
        },
        "custom_space_not_in_convention" => ResidualReason::CustomSpaceNotInConvention {
            raw: parse_u32(p1)?,
        },
        "facet_overflow_at_key" => ResidualReason::FacetOverflowAtKey {
            raw: parse_u32(p1)?,
        },
        "variadic_arity" => ResidualReason::VariadicArity {
            arity: parse_usize(p1)?,
        },
        "phi_fan_in_exceeds_predecessors" => ResidualReason::PhiFanInExceedsPredecessors {
            inputs: parse_usize(p1)?,
            predecessors: parse_usize(p2)?,
        },
        "memory_object_escaped" => ResidualReason::MemoryObjectEscaped,
        "indirect_target" => ResidualReason::IndirectTarget,
        "no_facet_coordinate" => ResidualReason::NoFacetCoordinate,
        "op_site_join_mismatch" => ResidualReason::OpSiteJoinMismatch {
            expected: parse_opcode(p1)?,
            found: parse_opcode(p2)?,
        },
        other => {
            return Err(malformed_owned(format!(
                "unknown residual reason {other:?}"
            )));
        }
    })
}

fn next_opt<'a>(cols: &mut impl Iterator<Item = &'a str>) -> io::Result<Option<&'a str>> {
    let s = cols.next().ok_or_else(|| malformed("missing column"))?;
    Ok((!s.is_empty()).then_some(s))
}

fn next_usize<'a>(cols: &mut impl Iterator<Item = &'a str>) -> io::Result<usize> {
    cols.next()
        .ok_or_else(|| malformed("missing column"))?
        .parse()
        .map_err(|_| malformed("non-numeric column"))
}

fn malformed(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn malformed_owned(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

// ================================================================================================
// Readers
// ================================================================================================

fn read_header(lines: &mut impl Iterator<Item = io::Result<String>>) -> io::Result<()> {
    lines
        .next()
        .ok_or_else(|| malformed("missing #version line"))??;
    lines
        .next()
        .ok_or_else(|| malformed("missing #schema line"))??;
    Ok(())
}

/// Read back a report written by [`OfflineSink::write_harvest`]. Round-trips
/// the four counts; does NOT reconstruct [`HarvestReport::is_conserved`] from
/// anything but those same counts, so a tampered file is caught the same way
/// a tampered in-memory report would be.
///
/// # Errors
/// I/O errors, or a malformed/missing schema line.
pub fn read_report(path: &Path) -> io::Result<HarvestReport> {
    let f = BufReader::new(File::open(path)?);
    let mut lines = f.lines();
    read_header(&mut lines)?;
    let data = lines
        .next()
        .ok_or_else(|| malformed("missing data line"))??;
    let mut cols = data.split('\t');
    let harvested = next_usize(&mut cols)?;
    let classified = next_usize(&mut cols)?;
    let residual = next_usize(&mut cols)?;
    let dropped = next_usize(&mut cols)?;
    Ok(HarvestReport {
        harvested,
        classified,
        residual,
        dropped,
    })
}

/// Read back the `Vec<FlatFact>` written by [`OfflineSink::write_harvest`].
/// Byte-for-byte reconstruction of every field — including
/// [`FactProvenance::value`], which v1 of this writer dropped (see the
/// module docs' "v1 → v2" note).
///
/// # Errors
/// I/O errors, or a malformed/missing schema line, an unknown `concern`/
/// `kind`/`opcode` string, or a non-numeric column.
pub fn read_facts(path: &Path) -> io::Result<Vec<FlatFact>> {
    let f = BufReader::new(File::open(path)?);
    let mut lines = f.lines();
    read_header(&mut lines)?;
    let mut out = Vec::new();
    for line in lines {
        let line = line?;
        let mut cols = line.split('\t');
        let id: u32 = cols
            .next()
            .ok_or_else(|| malformed("missing id"))?
            .parse()
            .map_err(|_| malformed("non-numeric id"))?;
        let discriminant: u16 = cols
            .next()
            .ok_or_else(|| malformed("missing space_discriminant"))?
            .parse()
            .map_err(|_| malformed("non-numeric space_discriminant"))?;
        let offset: u64 = cols
            .next()
            .ok_or_else(|| malformed("missing offset"))?
            .parse()
            .map_err(|_| malformed("non-numeric offset"))?;
        let size: u32 = cols
            .next()
            .ok_or_else(|| malformed("missing size"))?
            .parse()
            .map_err(|_| malformed("non-numeric size"))?;
        let concern = concern_from_str(cols.next().ok_or_else(|| malformed("missing concern"))?)
            .ok_or_else(|| malformed("unknown concern"))?;
        let kind = fact_kind_from_str(cols.next().ok_or_else(|| malformed("missing kind"))?)
            .ok_or_else(|| malformed("unknown kind"))?;
        let opcode = OpTag::parse(cols.next().ok_or_else(|| malformed("missing opcode"))?)
            .ok_or_else(|| malformed("unknown opcode"))?;
        let a: u64 = cols
            .next()
            .ok_or_else(|| malformed("missing a"))?
            .parse()
            .map_err(|_| malformed("non-numeric a"))?;
        let b: u64 = cols
            .next()
            .ok_or_else(|| malformed("missing b"))?
            .parse()
            .map_err(|_| malformed("non-numeric b"))?;
        let prov = parse_provenance_cols(&mut cols)?;
        out.push(FlatFact {
            id: FactId(id),
            at: facet_from_raw(discriminant, offset, size),
            concern,
            kind,
            opcode,
            a,
            b,
            prov,
        });
    }
    Ok(out)
}

/// Read back the residual rows written by [`OfflineSink::write_harvest`],
/// as plain `Vec<ResidualFact>` — NOT re-wrapped in a [`ResidualLedger`],
/// since [`ResidualLedger`] has no public "trust me, these shape ids are
/// already correct" constructor and re-deriving `shape_id` from
/// `reason.shape_id()` per row (rather than trusting the written value) is
/// the honest reconstruction; a caller who wants a ledger re-pushes each row
/// through [`ResidualLedger::push`], which computes `shape_id` itself.
///
/// # Errors
/// I/O errors, a malformed/missing schema line, an unknown `reason`/
/// `prefix_kind`/opcode-payload string, or a non-numeric column.
pub fn read_residuals(path: &Path) -> io::Result<Vec<ResidualFact>> {
    let f = BufReader::new(File::open(path)?);
    let mut lines = f.lines();
    read_header(&mut lines)?;
    let mut out = Vec::new();
    for line in lines {
        let line = line?;
        let mut cols = line.split('\t');
        let reason_name = cols.next().ok_or_else(|| malformed("missing reason"))?;
        let p1 = cols
            .next()
            .ok_or_else(|| malformed("missing reason_payload1"))?;
        let p2 = cols
            .next()
            .ok_or_else(|| malformed("missing reason_payload2"))?;
        let reason = parse_reason(reason_name, p1, p2)?;
        let at = parse_facet_cols(&mut cols)?;
        let at_prefix = parse_prefix_cols(&mut cols)?;
        let provenance = parse_provenance_cols(&mut cols)?;
        out.push(ResidualFact {
            shape_id: reason.shape_id(),
            reason,
            at,
            at_prefix,
            provenance,
        });
    }
    Ok(out)
}

/// Backend 3's credential plumbing — `S3Config::from_env` only. See the
/// module docs' SUBSTRATE RULING: no upload is implemented against this
/// config in this crate yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Config {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub endpoint_url: String,
    pub region: String,
}

impl S3Config {
    /// Reads `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` /
    /// `AWS_ENDPOINT_URL_S3` / `AWS_REGION`. `None` when ANY are unset or
    /// empty — partial credentials are refused rather than half-applied,
    /// matching the ruling's "fall back to local offline mode rather than
    /// failing": a caller sees `None` and falls back, never a config that
    /// silently omits the endpoint.
    #[must_use]
    #[expect(
        clippy::disallowed_methods,
        reason = "ruff/clippy.toml's disallowed-methods list is directory-scoped, not \
            Cargo-workspace-scoped, so it reaches this workspace-EXCLUDED crate even \
            though its reasons say 'in ty crates' — ruff_r2il has no ty::System trait \
            to route through; plain std::env::var is correct here"
    )]
    pub fn from_env() -> Option<Self> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Same resolution rule as [`Self::from_env`], over an injected lookup
    /// rather than the real process environment. `from_env` is a one-line
    /// wrapper over this; tests exercise this directly so no test needs to
    /// mutate real env state (`std::env::set_var`/`remove_var` require
    /// `unsafe` and this crate forbids `unsafe_code` — and mutating real env
    /// vars would make this test racy against any other test touching the
    /// same names, which a fake lookup sidesteps entirely).
    #[must_use]
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let get = |name: &str| -> Option<String> {
            let v = lookup(name)?;
            (!v.is_empty()).then_some(v)
        };
        Some(Self {
            access_key_id: get("AWS_ACCESS_KEY_ID")?,
            secret_access_key: get("AWS_SECRET_ACCESS_KEY")?,
            endpoint_url: get("AWS_ENDPOINT_URL_S3")?,
            region: get("AWS_REGION")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facet::{self, CustomSpaceTable};
    use r2il::Varnode;

    fn spaces() -> CustomSpaceTable {
        CustomSpaceTable::from_ids(std::iter::empty()).expect("empty set never overflows")
    }

    fn one_fact() -> FlatFact {
        let vn = Varnode::register(0x10, 8);
        let at = facet::project(&vn, &spaces()).expect("register space never overflows");
        FlatFact {
            id: FactId(0),
            at,
            concern: Concern::Values,
            kind: FactKind::OperandIn,
            opcode: OpTag::Copy,
            a: 7,
            b: 0,
            prov: FactProvenance {
                inst: Some(InstId(3)),
                block: Some(BlockId(9)),
                op_site: Some((0x4010, 2)),
                value: Some(ValueId(42)),
            },
        }
    }

    fn one_residual_with_payload() -> ResidualFact {
        let reason = ResidualReason::OpSiteJoinMismatch {
            expected: OpTag::IntAdd,
            found: OpTag::Copy,
        };
        ResidualFact {
            shape_id: reason.shape_id(),
            reason,
            at: {
                let vn = Varnode::register(0x20, 4);
                Some(facet::project(&vn, &spaces()).expect("register space never overflows"))
            },
            at_prefix: Some(FacetPrefix::SpaceOffset {
                discriminant: 1,
                offset: 0x20,
            }),
            provenance: FactProvenance {
                inst: Some(InstId(5)),
                block: Some(BlockId(1)),
                op_site: Some((0x4020, 0)),
                value: None,
            },
        }
    }

    fn one_residual_payload_free() -> ResidualFact {
        let reason = ResidualReason::MemoryObjectEscaped;
        ResidualFact {
            shape_id: reason.shape_id(),
            reason,
            at: None,
            at_prefix: None,
            provenance: FactProvenance {
                inst: None,
                block: None,
                op_site: None,
                value: None,
            },
        }
    }

    #[test]
    fn sanitise_source_replaces_every_non_filesystem_safe_byte() {
        assert_eq!(sanitise_source("bin/ls"), "bin_ls");
        assert_eq!(sanitise_source("a b:c"), "a_b_c");
        // anti-vacuity: a name with NO unsafe bytes is untouched, proving the
        // function does not rewrite everything unconditionally.
        assert_eq!(sanitise_source("stress_test-opt.v1"), "stress_test-opt.v1");
    }

    #[test]
    fn offline_sink_round_trips_the_report_counts() {
        let dir = std::env::temp_dir().join(format!("ruff_r2il_sink_test_{}", std::process::id()));
        let mut sink = OfflineSink::new(&dir);
        let facts = [one_fact()];
        let ledger = ResidualLedger::new();
        let report = HarvestReport {
            harvested: 5,
            classified: 3,
            residual: 2,
            dropped: 0,
        };

        sink.write_harvest("t/est", &facts, &ledger, &report)
            .expect("write must succeed");

        // sanitise_source is what the reader must also apply — proves the
        // path convention is documented behaviour, not an accident of the
        // writer's own internals.
        let report_path = sink
            .dir()
            .join(format!("{}.report.tsv", sanitise_source("t/est")));
        let read_back = read_report(&report_path).expect("read must succeed");
        assert_eq!(read_back, report);

        // anti-vacuity: a DIFFERENT report written to a DIFFERENT source
        // does not collide with or overwrite the first.
        let report2 = HarvestReport {
            harvested: 9,
            classified: 9,
            residual: 0,
            dropped: 0,
        };
        sink.write_harvest("t/est-2", &facts, &ledger, &report2)
            .expect("second write must succeed");
        let read_back = read_report(&report_path).expect("first file must be untouched");
        assert_eq!(
            read_back, report,
            "second write must not clobber the first source's file"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_facts_round_trips_every_field_including_value_id() {
        let dir =
            std::env::temp_dir().join(format!("ruff_r2il_sink_facts_test_{}", std::process::id()));
        let mut sink = OfflineSink::new(&dir);
        let fact = one_fact();
        let ledger = ResidualLedger::new();
        let report = HarvestReport::default();
        sink.write_harvest("bin", std::slice::from_ref(&fact), &ledger, &report)
            .expect("write must succeed");

        let facts_path = sink.dir().join("bin.facts.tsv");
        let read_back = read_facts(&facts_path).expect("read must succeed");
        assert_eq!(read_back.len(), 1);
        assert_eq!(
            read_back[0], fact,
            "every field, including prov.value, must round-trip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "ruff/clippy.toml's disallowed-methods list is directory-scoped, not \
            Cargo-workspace-scoped, so it reaches this workspace-EXCLUDED crate even \
            though its reasons say 'in ty crates' — ruff_r2il has no ty::System trait \
            to route through; plain std::fs is correct here"
    )]
    fn read_facts_refuses_an_unknown_opcode_rather_than_guessing() {
        let dir = std::env::temp_dir().join(format!(
            "ruff_r2il_sink_bad_opcode_test_{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir must succeed");
        let path = dir.join("bad.facts.tsv");
        std::fs::write(
            &path,
            "#version 2\n#schema id\tspace_discriminant\toffset\tsize\tconcern\tkind\topcode\ta\tb\tblock_addr\top_idx\tinst_id\tvalue_id\n\
             0\t1\t16\t8\tvalues\toperand_in\tnot_a_real_opcode\t7\t0\t\t\t\t\n",
        )
        .expect("write must succeed");

        let err = read_facts(&path).expect_err("an unknown opcode string must be refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_residuals_round_trips_a_two_opcode_payload_reason() {
        let dir = std::env::temp_dir().join(format!(
            "ruff_r2il_sink_residuals_test_{}",
            std::process::id()
        ));
        let mut sink = OfflineSink::new(&dir);
        let facts: [FlatFact; 0] = [];
        let mut ledger = ResidualLedger::new();
        ledger.push(one_residual_with_payload());
        let report = HarvestReport::default();
        sink.write_harvest("bin", &facts, &ledger, &report)
            .expect("write must succeed");

        let residuals_path = sink.dir().join("bin.residuals.tsv");
        let read_back = read_residuals(&residuals_path).expect("read must succeed");
        assert_eq!(read_back.len(), 1);
        assert_eq!(
            read_back[0],
            one_residual_with_payload(),
            "OpSiteJoinMismatch's two OpTag payload fields, at_prefix, and provenance must all round-trip"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_residuals_round_trips_a_payload_free_reason_with_no_facet() {
        let dir = std::env::temp_dir().join(format!(
            "ruff_r2il_sink_residuals_free_test_{}",
            std::process::id()
        ));
        let mut sink = OfflineSink::new(&dir);
        let facts: [FlatFact; 0] = [];
        let mut ledger = ResidualLedger::new();
        ledger.push(one_residual_payload_free());
        let report = HarvestReport::default();
        sink.write_harvest("bin", &facts, &ledger, &report)
            .expect("write must succeed");

        let residuals_path = sink.dir().join("bin.residuals.tsv");
        let read_back = read_residuals(&residuals_path).expect("read must succeed");
        assert_eq!(read_back.len(), 1);
        // anti-vacuity: explicitly confirm the `at`/`at_prefix` None-ness
        // round-tripped, not just that SOME row came back.
        assert!(read_back[0].at.is_none());
        assert!(read_back[0].at_prefix.is_none());
        assert_eq!(read_back[0], one_residual_payload_free());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn s3_config_from_lookup_refuses_partial_credentials() {
        use std::collections::HashMap;

        let full: HashMap<String, String> = HashMap::from([
            ("AWS_ACCESS_KEY_ID".to_string(), "ak".to_string()),
            ("AWS_SECRET_ACCESS_KEY".to_string(), "sk".to_string()),
            (
                "AWS_ENDPOINT_URL_S3".to_string(),
                "https://example.invalid".to_string(),
            ),
            ("AWS_REGION".to_string(), "auto".to_string()),
        ]);
        fn lookup(m: HashMap<String, String>) -> impl Fn(&str) -> Option<String> {
            move |name: &str| m.get(name).cloned()
        }

        // can-fire half: all four present resolves to Some.
        let got = S3Config::from_lookup(lookup(full.clone())).expect("all four vars must resolve");
        assert_eq!(got.access_key_id, "ak");
        assert_eq!(got.region, "auto");

        // can-stay-silent half, disable-run-verified: drop exactly ONE key
        // at a time and confirm the WHOLE config is refused — a vacuous
        // "checks something is set" test would miss this, since it only
        // checks the all-present case. Anti-vacuity: if this loop instead
        // asserted `is_some()`, it would fail immediately, proving the
        // assertion is load-bearing rather than trivially true.
        for missing in full.keys() {
            let mut partial = full.clone();
            partial.remove(missing);
            assert!(
                S3Config::from_lookup(lookup(partial)).is_none(),
                "missing {missing} must refuse the whole config, not just that field"
            );
        }

        // empty string is refused the same as absent — a caller exporting
        // `AWS_REGION=` should not silently produce a config with an empty
        // region.
        let mut blank_region = full.clone();
        blank_region.insert("AWS_REGION".to_string(), String::new());
        assert!(
            S3Config::from_lookup(lookup(blank_region)).is_none(),
            "an empty-string var must be treated as unset, not as a valid empty value"
        );

        // nothing set at all.
        assert!(
            S3Config::from_lookup(|_| None).is_none(),
            "no vars available must resolve to None"
        );
    }
}
