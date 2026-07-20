//! The Klickwege structure-parity oracle (transcode doctrine: "`MySQL` = value
//! parity, Klickwege = structure parity"). Prints a deterministic digest of
//! the UI-navigation plane harvested from a corpus — screens, navigation
//! edges (`navigates_to`), tab/view selections (`selects_view`), concept
//! bindings (`surfaces_concept`, resolved against a codebook + alias
//! convention), and per-screen control/handler surface counts
//! (`contains_control` / `handles_event`).
//!
//! Corpus-agnostic by construction, same discipline as `rekey_exam`: the
//! corpus and the resolution config (codebook rows, concept aliases) both
//! arrive as runtime config — no corpus tokens live in this file, and no
//! corpus data is ever committed.
//!
//! Run:
//! ```sh
//! cargo run -p ruff_spo_triplet --example nav_digest -- <nav-harvest.ndjson> <exam.conf>
//! ```
//!
//! Config format: identical directive language to `rekey_exam`'s exam
//! config (see that example's doc comment, or
//! `ruff_spo_triplet::exam_config::parse`'s doc comment, for the full
//! vocabulary) — this oracle only reads the `alias=`, `codebook=`, and
//! `region=` rows, since navigation-plane resolution needs no
//! verb/scope/surface/grammar convention. `region=<dock-token>:<region>`
//! rows bind a `docked_at` dock token to a region name for the `[regions]`
//! section; a dock token with no row renders `unmapped:<token>` instead of
//! dropping.
//!
//! Output format: see [`ruff_spo_triplet::build_nav_digest`]'s doc comment
//! for the exact, diffable digest shape — every section sorted
//! lexicographically and deduplicated, stable across runs regardless of the
//! harvest's triple order.

#![expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "the whole point of this example is to print the nav digest"
)]

use ruff_spo_triplet::{build_nav_digest, from_ndjson, parse};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(ndjson_path), Some(conf_path)) = (args.next(), args.next()) else {
        eprintln!("usage: nav_digest <nav-harvest.ndjson> <exam.conf>");
        std::process::exit(2);
    };
    let ndjson = std::fs::read_to_string(&ndjson_path).expect("read nav harvest ndjson");
    let conf = parse(&std::fs::read_to_string(&conf_path).expect("read exam config"));
    let triples = from_ndjson(&ndjson).expect("harvest validates against the closed vocab");

    print!("{}", build_nav_digest(&triples, &conf));
}
