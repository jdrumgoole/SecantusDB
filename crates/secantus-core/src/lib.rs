//! `secantus-core` — the pure-Rust operator engines of SecantusDB.
//!
//! This crate is **PyO3-free**. It ports the pure-Python operator engines
//! (`tasks/rust-rewrite-plan.md`) and operates entirely on `bson` values, with
//! no dependency on Python or CPython. That keeps it reusable: today the
//! `secantus-core-py` crate wraps it in a thin PyO3 layer to build the
//! `_secantus_core` extension (the `secantus-core` Python wheel); tomorrow a
//! standalone Rust `secantusdb` server can link the same engines directly.
//!
//! Ported so far: the six leaf engines (`sortkey`, `query::matches`,
//! `update::apply_update`, `expressions::evaluate`, `projection::apply_projection`,
//! `diff::compute_update_description`) plus the storage-independent aggregation
//! pipeline (`aggregate::apply_pipeline`). Each engine returns a "fall back to
//! Python" signal (`Fallback` / `Option::None` / `Result::Err`) for any construct
//! it can't reproduce byte-for-byte, so the two implementations never diverge —
//! they're parity-pinned by the `tests/test_rust_*_parity.py` suites.
//!
//! The Python/Rust boundary (the BSON "byte seam") and all GIL discipline live in
//! the sibling `secantus-core-py` crate, not here.

// Public API: the engines the PyO3 bindings (and a future standalone server)
// call directly.
pub mod aggregate;
pub mod collation;
pub mod diff;
pub mod expressions;
pub mod geo;
pub mod projection;
pub mod query;
pub mod sortkey;
pub mod update;

// Internal shared helpers — implementation details of the engines above, not
// part of the crate's public surface (kept crate-private so their deliberate
// `Result<_, ()>` "defer" signals stay internal rather than public API).
mod decimal;
mod densify;
mod fill;
mod group;
mod numeric;
pub mod order;
mod paths;
mod regexutil;
mod windowfields;

// Re-export the read-only dotted-path helpers (only) so the Rust storage layer's
// index-key builders can resolve `key_spec` fields against documents, mirroring
// how `secantus.storage` imports from `secantus.paths`. The module itself stays
// private — its `set_path` / `unset_path` use deliberate `Result<_, ()>` "defer"
// signals that shouldn't become public API (clippy::result_unit_err).
pub use paths::{get_path, get_path_values, has_path};

// Re-export the `$group` field-reference pushdown (only) so the command layer
// can decode just the top-level fields a `$group` reads from wide documents
// ahead of the stage, instead of materializing every field. The `group` module
// itself stays private.
pub use group::referenced_top_level_fields;
