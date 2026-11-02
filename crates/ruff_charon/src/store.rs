//! The offline Codegraph store: per crate, the relation rows, the residuals and
//! the digests of the source files the rows came from.
//!
//! The artifact discipline is `ruff_r2il::sink`'s (`#version N` + `#schema
//! <names>`, columns appended never reordered, readers key off `#schema`
//! names). `ruff_r2il::sink::RefinedTruthSink` itself is typed to R2IL's
//! `FlatFact`s and is not reused: these rows are call relations, not R2IL.
//!
//! # Incremental updates
//!
//! Charon translates a whole crate, so the unit of update is a crate. A crate
//! is stale when one of its recorded source files changed or vanished
//! ([`Store::stale`]). Refreshing a crate replaces all three of its artifacts;
//! nothing of the old version survives. A crate that depends on a refreshed
//! crate must be refreshed too ([`with_dependents`]): its call rows name items
//! of the dependency, and [`Store::dangling`] reports any call row whose callee
//! is a function of a stored crate that no stored crate defines any more.
//!
//! A file added to a crate is only detected through the file that declares it
//! (`mod x;`), which changes with it.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{GRADE, Llbc, relations};

/// The version of the rows artifact this crate writes and reads.
pub const ROWS_VERSION: u32 = 1;
const ROWS_SCHEMA: &[&str] = &[
    "predicate",
    "subject",
    "object",
    "grade",
    "file",
    "start",
    "end",
];
const FILES_VERSION: u32 = 1;
const FILES_SCHEMA: &[&str] = &["path", "fnv64"];
const RESIDUALS_VERSION: u32 = 1;
const RESIDUALS_SCHEMA: &[&str] = &["reason", "subject"];

/// One stored relation row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Row {
    pub predicate: String,
    pub subject: String,
    pub object: String,
    pub grade: String,
    pub site: Option<(String, usize, usize)>,
}

/// FNV-1a 64 over bytes, the digest `sag-ra-probe` uses for bodies and files.
#[must_use]
pub fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The source files of `crate_name` as Charon read them, with their digests.
/// Files whose contents Charon did not keep are not listed.
#[must_use]
pub fn crate_files(llbc: &Llbc, crate_name: &str) -> Vec<(String, u64)> {
    let mut files: Vec<_> = llbc
        .krate
        .files
        .iter()
        .filter(|f| f.crate_name == crate_name)
        .filter_map(|f| match (&f.name, &f.contents) {
            (charon_lib::ast::FileName::Local(p), Some(text)) => {
                Some((p.display().to_string(), fnv64(text.as_bytes())))
            }
            _ => None,
        })
        .collect();
    files.sort();
    files.dedup();
    files
}

fn header(out: &mut impl Write, version: u32, schema: &[&str]) -> io::Result<()> {
    writeln!(out, "#version {version}")?;
    writeln!(out, "#schema {}", schema.join("\t"))
}

/// Read an artifact: check the version, map the `#schema` names to columns,
/// and return each data line as a name → value map.
fn read_table(
    path: &Path,
    max_version: u32,
    need: &[&str],
) -> io::Result<Vec<BTreeMap<String, String>>> {
    let bad = |m: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{}: {m}", path.display()),
        )
    };
    let mut version = None;
    let mut names: Option<Vec<String>> = None;
    let mut rows = Vec::new();
    for line in BufReader::new(fs::File::open(path)?).lines() {
        let line = line?;
        if let Some(v) = line.strip_prefix("#version ") {
            version = Some(
                v.parse::<u32>()
                    .map_err(|e| bad(format!("bad version: {e}")))?,
            );
        } else if let Some(s) = line.strip_prefix("#schema ") {
            names = Some(s.split('\t').map(str::to_owned).collect());
        } else if !line.is_empty() {
            let names = names
                .as_ref()
                .ok_or_else(|| bad("data before #schema".into()))?;
            let cols: Vec<&str> = line.split('\t').collect();
            if cols.len() != names.len() {
                return Err(bad(format!(
                    "{} columns, schema has {}",
                    cols.len(),
                    names.len()
                )));
            }
            rows.push(
                names
                    .iter()
                    .cloned()
                    .zip(cols.iter().map(|c| (*c).to_owned()))
                    .collect(),
            );
        }
    }
    match version {
        Some(v) if v <= max_version => {}
        Some(v) => return Err(bad(format!("version {v} is newer than {max_version}"))),
        None => return Err(bad("no #version".into())),
    }
    let names = names.unwrap_or_default();
    for n in need {
        if !names.iter().any(|x| x == n) {
            return Err(bad(format!("schema lacks column {n}")));
        }
    }
    Ok(rows)
}

/// A directory of per-crate artifacts.
#[derive(Debug, Clone)]
pub struct Store {
    dir: PathBuf,
}

impl Store {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, crate_name: &str, suffix: &str) -> PathBuf {
        self.dir.join(format!("{crate_name}.{suffix}"))
    }

    /// Write `path` through a temporary file and a rename, so a reader never
    /// sees half an artifact.
    fn replace(path: &Path, body: &[u8]) -> io::Result<()> {
        // A temporary file of its own per call: two imports of one crate must
        // not write into each other's file, or one of them publishes the
        // other's half-written bytes (or fails when it was renamed away).
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact");
        let tmp = path.with_file_name(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    }

    /// Import (or refresh) one crate from its LLBC: replaces its rows,
    /// residuals and file digests. Returns the relation receipt; an
    /// unconserved receipt is an error and nothing is written.
    ///
    /// # Errors
    /// I/O errors, or `InvalidData` when the receipt does not conserve.
    #[expect(clippy::disallowed_methods, reason = "see `replace`")]
    pub fn import(&self, llbc: &Llbc, crate_name: &str) -> io::Result<crate::Receipt> {
        let (rows, residual, receipt) = relations(&llbc.krate, crate_name);
        if !receipt.conserved() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{receipt:?}"),
            ));
        }
        fs::create_dir_all(&self.dir)?;
        let mut out = Vec::new();
        header(&mut out, ROWS_VERSION, ROWS_SCHEMA)?;
        for r in &rows {
            let (f, s, e) = r
                .site
                .as_ref()
                .map_or(("-".to_owned(), "-".to_owned(), "-".to_owned()), |s| {
                    (s.file.clone(), s.start.to_string(), s.end.to_string())
                });
            writeln!(
                out,
                "{}\t{}\t{}\t{GRADE}\t{f}\t{s}\t{e}",
                r.predicate, r.subject, r.object
            )?;
        }
        let mut res = Vec::new();
        header(&mut res, RESIDUALS_VERSION, RESIDUALS_SCHEMA)?;
        for r in &residual {
            writeln!(res, "{}\t{}", r.reason, r.subject)?;
        }
        let mut files = Vec::new();
        header(&mut files, FILES_VERSION, FILES_SCHEMA)?;
        for (p, d) in crate_files(llbc, crate_name) {
            writeln!(files, "{p}\t{d:016x}")?;
        }
        // Files last: a crate whose digests are present has its rows.
        Self::replace(&self.path(crate_name, "rows.tsv"), &out)?;
        Self::replace(&self.path(crate_name, "residuals.tsv"), &res)?;
        Self::replace(&self.path(crate_name, "files.tsv"), &files)?;
        Ok(receipt)
    }

    /// The crates in the store, sorted.
    ///
    /// # Errors
    /// I/O errors reading the directory.
    #[expect(clippy::disallowed_methods, reason = "see `replace`")]
    pub fn crates(&self) -> io::Result<Vec<String>> {
        let mut out: Vec<String> = fs::read_dir(&self.dir)?
            .filter_map(Result::ok)
            .filter_map(|e| {
                e.file_name()
                    .to_str()?
                    .strip_suffix(".files.tsv")
                    .map(str::to_owned)
            })
            .collect();
        out.sort();
        Ok(out)
    }

    /// One crate's rows, read by schema names.
    ///
    /// # Errors
    /// I/O or format errors.
    pub fn rows(&self, crate_name: &str) -> io::Result<Vec<Row>> {
        let table = read_table(
            &self.path(crate_name, "rows.tsv"),
            ROWS_VERSION,
            ROWS_SCHEMA,
        )?;
        Ok(table
            .into_iter()
            .map(|m| {
                let site = (m["file"] != "-").then(|| {
                    (
                        m["file"].clone(),
                        m["start"].parse().unwrap_or(0),
                        m["end"].parse().unwrap_or(0),
                    )
                });
                Row {
                    predicate: m["predicate"].clone(),
                    subject: m["subject"].clone(),
                    object: m["object"].clone(),
                    grade: m["grade"].clone(),
                    site,
                }
            })
            .collect())
    }

    /// Every row of every crate, as a set: the form two stores are compared in.
    ///
    /// # Errors
    /// I/O or format errors.
    pub fn all_rows(&self) -> io::Result<BTreeSet<Row>> {
        let mut out = BTreeSet::new();
        for c in self.crates()? {
            out.extend(self.rows(&c)?);
        }
        Ok(out)
    }

    /// One crate's recorded files and digests.
    ///
    /// # Errors
    /// I/O or format errors.
    pub fn files(&self, crate_name: &str) -> io::Result<Vec<(String, u64)>> {
        let table = read_table(
            &self.path(crate_name, "files.tsv"),
            FILES_VERSION,
            FILES_SCHEMA,
        )?;
        table
            .into_iter()
            .map(|m| {
                u64::from_str_radix(&m["fnv64"], 16)
                    .map(|d| (m["path"].clone(), d))
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
            })
            .collect()
    }

    /// The crates with a recorded file that changed or vanished under `root`.
    ///
    /// # Errors
    /// I/O or format errors reading the store.
    pub fn stale(&self, root: &Path) -> io::Result<BTreeSet<String>> {
        let mut out = BTreeSet::new();
        for c in self.crates()? {
            for (p, d) in self.files(&c)? {
                match fs::read(root.join(&p)) {
                    Ok(bytes) if fnv64(&bytes) == d => {}
                    _ => {
                        out.insert(c.clone());
                        break;
                    }
                }
            }
        }
        Ok(out)
    }

    /// Call rows whose callee is a function of a stored crate that no stored
    /// crate defines: edges left behind by an incomplete refresh. A call
    /// resolved through a known trait impl names the trait (`<trait> via
    /// TraitImpl`), not a function, so it is not checked here.
    ///
    /// # Errors
    /// I/O or format errors.
    pub fn dangling(&self) -> io::Result<Vec<Row>> {
        let crates = self.crates()?;
        let all = self.all_rows()?;
        let defined: BTreeSet<&str> = all
            .iter()
            .filter(|r| r.predicate == "DEFINES")
            .map(|r| r.subject.as_str())
            .collect();
        Ok(all
            .iter()
            .filter(|r| r.predicate == "CALLS_RESOLVED" && !r.object.contains(" via "))
            .filter(|r| {
                let owner = r.object.split("::").next().unwrap_or("");
                crates.iter().any(|c| c == owner) && !defined.contains(r.object.as_str())
            })
            .cloned()
            .collect())
    }
}

/// `stale` plus every crate that depends on one of them, transitively.
/// `deps` lists (dependent, dependency) pairs, e.g. from `cargo metadata`.
#[must_use]
pub fn with_dependents(stale: &BTreeSet<String>, deps: &[(String, String)]) -> BTreeSet<String> {
    let mut out = stale.clone();
    loop {
        let before = out.len();
        for (dependent, dependency) in deps {
            if out.contains(dependency) {
                out.insert(dependent.clone());
            }
        }
        if out.len() == before {
            return out;
        }
    }
}
