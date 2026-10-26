//! A rule's node trees (RP-203): PostgreSQL 15.19's parse analysis of a
//! view's query or a rule's actions, written by `nodeToString` as
//! `pg_rewrite.ev_action` / `ev_qual` hold them.
//!
//! The analysis is a transcription of PostgreSQL's parser (`analyze.c`,
//! `parse_*.c`), run over the raw parse tree `libpg_query` gives and the
//! server's own catalogs ([`catalog::CatalogData`]). The output is
//! `outfuncs.c`'s, so the trees embed what PostgreSQL's do: catalog oids,
//! attribute numbers, type modifiers, collations and the source offsets of
//! every token.
//!
//! What it does not transcribe it refuses: an analysis that meets a
//! construct it does not know returns an error, and the server records no
//! tree rather than one that is wrong.

// The transcription keeps PostgreSQL's branch structure, so that each
// function reads against its C original: branches with the same body, and
// nested ifs the C does not merge, stay as they are written there.
#![allow(
    clippy::if_same_then_else,
    clippy::collapsible_if,
    clippy::collapsible_match,
    clippy::needless_range_loop
)]

mod agg_check;
pub mod catalog;
mod clause;
mod coerce;
mod collate;
mod datum;
mod expr;
mod func;
mod funcapi;
mod nodefuncs;
pub mod nodes;
mod oper;
pub mod out;
mod parse;
mod relation;
pub mod system;
mod target;
#[cfg(test)]
mod tests;
mod types;

pub use catalog::{Catalog, CatalogData};
pub use out::{node_to_string, queries_to_string};

/// Why an analysis produced no tree.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalysisError(pub String);

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

pub(crate) type R<T> = Result<T, AnalysisError>;

pub(crate) fn unsupported<T>(what: impl Into<String>) -> R<T> {
    Err(AnalysisError(what.into()))
}

/// A rule's stored trees, as `pg_rewrite` holds them.
#[derive(Clone, Debug, PartialEq)]
pub struct RuleTrees {
    /// `ev_qual`: `<>` without a qualification.
    pub ev_qual: String,
    /// `ev_action`: the action queries.
    pub ev_action: String,
}

pub use parse::{
    analyze_create_matview, analyze_create_rule, analyze_create_view, split_initdb_file,
    AnalysisContext,
};
