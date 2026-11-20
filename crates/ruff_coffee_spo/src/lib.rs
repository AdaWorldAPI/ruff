//! `ruff_coffee_spo` - a minimal CoffeeScript frontend for config-call
//! registrations.
//!
//! Zammad's legacy frontend registers its screens with calls such as
//! `App.Config.set('ticket/zoom/:ticket_id', TicketZoomRouter, 'Routes')`.
//! This crate harvests every call to one dotted callee as a [`CoffeeCall`]
//! whose arguments are lowered to [`CoffeeArg`]. It is not a CoffeeScript
//! parser: a tokenizer ([`tokenizer`]) and a call finder ([`calls`]) are all
//! there is.
//!
//! # Conservation
//!
//! In every [`CoffeeScanReport`]:
//! `calls_found == calls_fully_literal + calls_with_other`, and
//! `calls_found == calls.len()`. A call is fully literal when every argument is
//! [`CoffeeArg::Str`], [`CoffeeArg::Path`] or [`CoffeeArg::Num`] (a call
//! without arguments is vacuously fully literal). `implicit_calls` counts the
//! calls written without parentheses and is a subset of `calls_found`. A call
//! whose closing `)` never comes counts only in `unterminated` and is not
//! returned. A file that cannot be read counts in no figure.
//!
//! No SPO triples are emitted: the shared `Predicate` set has no variant for
//! config-call registrations, and adding one is outside this crate.

pub mod calls;
pub mod tokenizer;

pub use calls::{
    CoffeeArg, CoffeeCall, CoffeeScanReport, coffee_files, extract_calls, extract_calls_from_source,
};
