//! A one-way projection of ruff's source harvest into the Codegraph store.
//!
//! The Ruby/Rails frontend fills a `ruff_spo_triplet::ModelGraph` and an
//! ordered event stream. This crate writes them as the files
//! `lance-graph-codegraph` reads (`<ns>.rows.tsv`, `.files.tsv`,
//! `.residuals.tsv`), plus an order lane (`<ns>.events.tsv`) and a receipt
//! (`<ns>.receipt.json`). The file format is the only interface: nothing here
//! depends on lance-graph or reads a store back into a graph.

pub mod account;
pub mod cell;
pub mod config;
pub mod error;
pub mod events;
pub mod fnv;
mod fsio;
pub mod imports;
pub mod order;
pub mod project;
pub mod receipt;
pub mod rows;

pub use config::{Config, Frontend};
pub use error::Error;
pub use order::{Relation, relation};
pub use project::{Projection, project};
pub use receipt::Receipt;
