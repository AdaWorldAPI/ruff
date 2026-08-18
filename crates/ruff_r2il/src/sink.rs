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

use std::fmt;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::furnace::{Concern, FlatFact, HarvestReport};
use crate::slag::ResidualLedger;

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
    /// that [`read_report`] needs, without a filesystem side effect.
    ///
    /// **Honesty note:** only [`read_report`] exists so far — a facts/
    /// residuals reader (reconstructing [`FlatFact`]/`ResidualLedger` rows
    /// from the TSV, including `VarnodeFacet` from its three written
    /// columns) is real remaining scope, not implemented here. Do not
    /// assume a `read_facts`/`read_residuals` exists.
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

const FACTS_VERSION: u32 = 1;
const FACTS_SCHEMA: &str = "id\tspace_discriminant\toffset\tsize\tconcern\tkind\topcode\ta\tb\tblock_addr\top_idx\tinst_id";

const RESIDUALS_VERSION: u32 = 1;
const RESIDUALS_SCHEMA: &str = "reason\tspace_discriminant\toffset\tsize";

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
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                fact.id.0,
                fact.at.space_discriminant(),
                fact.at.offset(),
                fact.at.size(),
                concern_str(fact.concern),
                fact.kind.as_str(),
                fact.opcode.as_str(),
                fact.a,
                fact.b,
                fact.prov
                    .block
                    .map_or_else(String::new, |b| b.0.to_string()),
                fact.prov
                    .op_site
                    .map_or_else(String::new, |(addr, idx)| format!("{addr}:{idx}")),
                fact.prov.inst.map_or_else(String::new, |i| i.0.to_string()),
            )?;
        }
        w.flush()?;

        let mut w = BufWriter::new(File::create(self.path_for(source, "residuals.tsv"))?);
        writeln!(w, "#version {RESIDUALS_VERSION}")?;
        writeln!(w, "#schema {RESIDUALS_SCHEMA}")?;
        for row in residuals.rows() {
            writeln!(
                w,
                "{}\t{}\t{}\t{}",
                row.reason.as_str(),
                row.at
                    .map(|f| f.space_discriminant())
                    .map_or_else(String::new, |d| d.to_string()),
                row.at
                    .map(|f| f.offset())
                    .map_or_else(String::new, |o| o.to_string()),
                row.at
                    .map(|f| f.size())
                    .map_or_else(String::new, |s| s.to_string()),
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
    let _version = lines
        .next()
        .ok_or_else(|| malformed("missing #version line"))??;
    let _schema = lines
        .next()
        .ok_or_else(|| malformed("missing #schema line"))??;
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

fn next_usize<'a>(cols: &mut impl Iterator<Item = &'a str>) -> io::Result<usize> {
    cols.next()
        .ok_or_else(|| malformed("missing column"))?
        .parse()
        .map_err(|_| malformed("non-numeric column"))
}

fn malformed(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
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
    use crate::furnace::{FactId, FactKind};
    use crate::ore::{FactProvenance, OpTag};
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
