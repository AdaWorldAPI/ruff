use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use ruff_ruby_spo::extract_tree_with;

use crate::account;
use crate::config::{Config, Frontend};
use crate::error::Error;
use crate::events::{self, Events};
use crate::fnv::{fnv64, hex};
use crate::fsio;
use crate::imports;
use crate::receipt::{self, Cardinality, DIRECTION, Receipt, Snapshot, lane};
use crate::rows::{self, Builder, Reason};

pub const ROWS_VERSION: u32 = 2;
pub const FILES_VERSION: u32 = 1;
pub const RESIDUALS_VERSION: u32 = 1;

const ROWS_SCHEMA: &str = "predicate\tsubject\tobject\tgrade\tfile\tstart\tend";
const FILES_SCHEMA: &str = "path\tfnv64";
const RESIDUALS_SCHEMA: &str = "reason\tsubject";

/// A projection, built in memory and checked, not yet written.
#[derive(Debug)]
pub struct Projection {
    pub namespace: String,
    pub rows_tsv: String,
    pub files_tsv: String,
    pub residuals_tsv: String,
    /// `None` when the order lane did not run.
    pub events_tsv: Option<String>,
    pub receipt: Receipt,
}

impl Projection {
    /// Run the lanes the config asks for and check conservation.
    pub fn build(config: &Config) -> Result<Self, Error> {
        let Frontend::RubyRails = config.frontend;
        let ns = config.namespace.as_str();

        // The source snapshot: every Ruby file the harvest reads.
        let mut files: Vec<(String, u64, Option<String>)> = Vec::new();
        for path in fsio::ruby_files(&config.root) {
            let bytes = fsio::read(&path)?;
            let relative = path
                .strip_prefix(&config.root)
                .ok()
                .and_then(Path::to_str)
                .map(|p| p.replace('\\', "/"))
                .ok_or_else(|| Error::Config {
                    path: config.root.display().to_string(),
                    message: format!(
                        "key `root`: {} is not valid UTF-8 or escapes the root",
                        path.display()
                    ),
                })?;
            files.push((
                format!("{}/{relative}", config.root_rel),
                fnv64(&bytes),
                String::from_utf8(bytes).ok(),
            ));
        }

        let mut builder = Builder::new(ns)?;
        let graph = if config.structural {
            let graph = extract_tree_with(&config.root, ns);
            rows::structural(&graph, &mut builder)?;
            Some(graph)
        } else {
            None
        };

        let (edges, imports_digest) = match &config.imports {
            Some(path) => {
                let text = fsio::read_text(path)?;
                let edges = imports::parse(&path.display().to_string(), &text)?;
                imports::project(ns, &edges, &mut builder)?;
                (Some(edges), Some(hex(fnv64(text.as_bytes()))))
            }
            None => (None, None),
        };
        let built = builder.finish();
        let conservation = account::check(graph.as_ref(), edges.as_ref(), &built)?;

        let events = if config.order {
            let mut events = Events::default();
            for (_, _, text) in &files {
                if let Some(text) = text {
                    events.add_source(ns, text)?;
                }
            }
            Some(events)
        } else {
            None
        };

        let mut rows_tsv = format!("#version {ROWS_VERSION}\n#schema {ROWS_SCHEMA}\n");
        let mut per_predicate = BTreeMap::new();
        for row in &built.rows {
            let _ = writeln!(
                rows_tsv,
                "{}\t{}\t{}\t{}\t-\t-\t-",
                row.predicate, row.subject, row.object, row.grade
            );
            *per_predicate
                .entry(row.predicate.to_owned())
                .or_insert(0u32) += 1;
        }

        let mut files_tsv = format!("#version {FILES_VERSION}\n#schema {FILES_SCHEMA}\n");
        let mut digest_input = String::new();
        for (path, digest, _) in &files {
            let line = format!("{path}\t{}\n", hex(*digest));
            files_tsv.push_str(&line);
            digest_input.push_str(&line);
        }

        let mut residuals_tsv =
            format!("#version {RESIDUALS_VERSION}\n#schema {RESIDUALS_SCHEMA}\n");
        for residual in &built.residuals {
            let _ = writeln!(
                residuals_tsv,
                "{}\t{}",
                residual.reason.as_str(),
                residual.subject
            );
        }
        let residual_counts = Reason::ALL
            .iter()
            .map(|reason| {
                let n = built
                    .residuals
                    .iter()
                    .filter(|r| r.reason == *reason)
                    .count();
                (
                    reason.as_str().to_owned(),
                    u32::try_from(n).unwrap_or(u32::MAX),
                )
            })
            .collect();

        let mut formats = BTreeMap::from([
            ("files".to_owned(), FILES_VERSION),
            ("receipt".to_owned(), receipt::VERSION),
            ("residuals".to_owned(), RESIDUALS_VERSION),
            ("rows".to_owned(), ROWS_VERSION),
        ]);
        if events.is_some() {
            formats.insert("events".to_owned(), events::VERSION);
        }

        let receipt = Receipt {
            cardinality: Cardinality {
                events: events.as_ref().map(Events::counts).unwrap_or_default(),
                residuals: residual_counts,
                rows: per_predicate,
            },
            conservation,
            direction: DIRECTION.to_owned(),
            formats,
            imports_fnv64: imports_digest,
            lanes: [
                ("imports", config.imports.is_some()),
                ("order", config.order),
                ("structural", config.structural),
            ]
            .into_iter()
            .map(|(name, on)| (name.to_owned(), lane(on)))
            .collect(),
            namespace: ns.to_owned(),
            not_projected: graph
                .as_ref()
                .map(account::not_projected)
                .unwrap_or_default(),
            snapshot: Snapshot {
                digest: hex(fnv64(digest_input.as_bytes())),
                files: u32::try_from(files.len()).unwrap_or(u32::MAX),
            },
        };

        Ok(Self {
            namespace: config.namespace.clone(),
            rows_tsv,
            files_tsv,
            residuals_tsv,
            events_tsv: events.as_ref().map(Events::render),
            receipt,
        })
    }

    /// Write the store files into `out`, the receipt last. An events file left
    /// by an earlier run is removed when the order lane did not run.
    pub fn write(&self, out: &Path) -> Result<(), Error> {
        fsio::create_dir_all(out)?;
        let file = |suffix: &str| out.join(format!("{}.{suffix}", self.namespace));
        fsio::write(&file("rows.tsv"), self.rows_tsv.as_bytes())?;
        fsio::write(&file("files.tsv"), self.files_tsv.as_bytes())?;
        fsio::write(&file("residuals.tsv"), self.residuals_tsv.as_bytes())?;
        match &self.events_tsv {
            Some(text) => fsio::write(&file("events.tsv"), text.as_bytes())?,
            None => fsio::remove_if_present(&file("events.tsv"))?,
        }
        fsio::write(&file("receipt.json"), self.receipt.render().as_bytes())
    }
}

/// Load `config_path`, project, and write into `out`.
pub fn project(config_path: &Path, out: &Path) -> Result<Receipt, Error> {
    let config = Config::load(config_path)?;
    let projection = Projection::build(&config)?;
    projection.write(out)?;
    Ok(projection.receipt)
}
