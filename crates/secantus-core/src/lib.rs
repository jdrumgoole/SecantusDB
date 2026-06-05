//! `_secantus_core` — the Rust core of SecantusDB, exposed to Python via PyO3.
//!
//! Phase 1 of the rewrite (tasks/rust-rewrite-plan.md): the first leaf engine,
//! `sortkey`, behind the **fat byte seam** described in §3 — values cross the
//! Python/Rust boundary as BSON bytes (a one-key document `{"v": <value>}`),
//! never as marshalled per-field Python objects. This keeps the seam aligned
//! with SecantusDB's "documents are opaque BSON blobs" design and avoids the
//! type-fidelity and per-call-conversion costs of value-by-value marshalling.
//!
//! The Python side (`secantus.sortkey`) is a shim that delegates here when
//! enabled (`SECANTUS_RUST_SORTKEY=1`) and otherwise runs the pure-Python
//! implementation, so this module is purely additive until we flip the default.

// PyO3 0.22's #[pyfunction] expansion inserts an identity `.into()` on the
// return type that clippy flags as a useless conversion; it's a macro artifact,
// not our code. Suppress at the crate level until we move to a PyO3 that fixed it.
#![allow(clippy::useless_conversion)]

use bson::Document;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

mod collation;
mod diff;
mod expressions;
mod numeric;
mod paths;
mod projection;
mod query;
mod sortkey;
mod update;

/// Decode the one-key wrapper document and hand back the wrapped value.
fn unwrap_value(doc_bytes: &[u8]) -> PyResult<bson::Bson> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid BSON wrapper: {e}")))?;
    doc.get("v")
        .cloned()
        .ok_or_else(|| PyValueError::new_err("wrapper document missing key 'v'"))
}

fn to_pybytes(py: Python<'_>, bytes: Vec<u8>) -> Py<PyBytes> {
    PyBytes::new_bound(py, &bytes).unbind()
}

/// `sortkey.encode_value(value)` — input is `bson.encode({"v": value})`.
#[pyfunction]
fn sortkey_encode_value(py: Python<'_>, doc_bytes: &[u8]) -> PyResult<Py<PyBytes>> {
    let value = unwrap_value(doc_bytes)?;
    let out = sortkey::encode_value(&value).map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(to_pybytes(py, out))
}

/// `sortkey.encode_value_directed(value, direction)`.
#[pyfunction]
fn sortkey_encode_value_directed(
    py: Python<'_>,
    doc_bytes: &[u8],
    direction: i32,
) -> PyResult<Py<PyBytes>> {
    let value = unwrap_value(doc_bytes)?;
    let out = sortkey::encode_value_directed(&value, direction)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(to_pybytes(py, out))
}

/// `query.matches(doc, query)` over BSON bytes. Returns `None` to signal the
/// caller should fall back to the pure-Python matcher (the query uses a feature
/// not ported yet: collation, `$expr`, `$jsonSchema`, geo, regex, `$all`, …).
#[pyfunction]
fn query_matches(
    doc_bytes: &[u8],
    query_bytes: &[u8],
    vars_bytes: &[u8],
    collation_bytes: &[u8],
) -> PyResult<Option<bool>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let query: Document = bson::from_slice(query_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid query BSON: {e}")))?;
    let vars: Document = bson::from_slice(vars_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid vars BSON: {e}")))?;
    // collation is a {strength, caseLevel, numericOrdering} doc, or {} for none.
    let coll_doc: Document = bson::from_slice(collation_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid collation BSON: {e}")))?;
    let coll = collation::parse(&coll_doc);
    // Ok(b) -> Some(b) (a real result); Err(Fallback) -> None (defer to Python).
    Ok(query::matches(&doc, &query, &vars, coll.as_ref()).ok())
}

/// `expressions.evaluate(expr, doc, vars)` over BSON bytes. `expr` and the
/// result are wrapped as `{"e": ...}` / `{"r": ...}` (BSON needs a document
/// envelope for non-document values). Returns the `{"r": ...}` bytes, or `None`
/// to fall back to the pure-Python evaluator (any operator/value not ported).
#[pyfunction]
fn evaluate(
    py: Python<'_>,
    doc_bytes: &[u8],
    expr_bytes: &[u8],
    vars_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let expr_wrap: Document = bson::from_slice(expr_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid expr BSON: {e}")))?;
    let vars: Document = bson::from_slice(vars_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid vars BSON: {e}")))?;
    let expr = expr_wrap
        .get("e")
        .ok_or_else(|| PyValueError::new_err("expr wrapper missing key 'e'"))?;
    match expressions::evaluate(&doc, expr, &vars) {
        Ok(value) => {
            let mut wrap = Document::new();
            wrap.insert("r".to_string(), value);
            let mut buf = Vec::new();
            wrap.to_writer(&mut buf)
                .map_err(|e| PyValueError::new_err(format!("encode failed: {e}")))?;
            Ok(Some(to_pybytes(py, buf)))
        }
        Err(expressions::Fallback) => Ok(None),
    }
}

/// `update.apply_update(doc, update, is_upsert=...)` over BSON bytes (update is
/// an operator/replacement document — pipeline updates stay in Python). Returns
/// the new document's BSON bytes, or `None` to fall back to the pure-Python
/// `apply_update` (positional ops, array filters, `$currentDate`,
/// `$min`/`$max`/`$pull`/`$addToSet`/`$bit`, Decimal128 arithmetic, or any
/// error condition so Python raises the exact `UpdateError`).
#[pyfunction]
fn apply_update(
    py: Python<'_>,
    doc_bytes: &[u8],
    update_bytes: &[u8],
    is_upsert: bool,
) -> PyResult<Option<Py<PyBytes>>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let update: Document = bson::from_slice(update_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid update BSON: {e}")))?;
    match update::apply_update(&doc, &update, is_upsert) {
        Ok(new) => {
            let mut buf = Vec::new();
            new.to_writer(&mut buf)
                .map_err(|e| PyValueError::new_err(format!("encode failed: {e}")))?;
            Ok(Some(to_pybytes(py, buf)))
        }
        Err(update::Fallback) => Ok(None),
    }
}

/// `projection.apply_projection(doc, spec)` over BSON bytes. Returns the
/// projected document's bytes, or `None` to fall back to pure Python (mixed
/// inclusion/exclusion which Python raises on, nested-document specs, unusual
/// `$slice` args, or a `$elemMatch` sub-filter the matcher defers).
#[pyfunction]
fn apply_projection(
    py: Python<'_>,
    doc_bytes: &[u8],
    spec_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let spec: Document = bson::from_slice(spec_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid spec BSON: {e}")))?;
    match projection::apply_projection(&doc, &spec) {
        Ok(out) => {
            let mut buf = Vec::new();
            out.to_writer(&mut buf)
                .map_err(|e| PyValueError::new_err(format!("encode failed: {e}")))?;
            Ok(Some(to_pybytes(py, buf)))
        }
        Err(projection::Fallback) => Ok(None),
    }
}

/// `diff.compute_update_description(pre, post)` over BSON bytes. Returns the
/// `{updatedFields, removedFields, truncatedArrays}` document's bytes, or `None`
/// to fall back to pure Python (Decimal128 / exotic values).
#[pyfunction]
fn compute_update_description(
    py: Python<'_>,
    pre_bytes: &[u8],
    post_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let pre: Document = bson::from_slice(pre_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid pre BSON: {e}")))?;
    let post: Document = bson::from_slice(post_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid post BSON: {e}")))?;
    match diff::compute_update_description(&pre, &post) {
        Ok(out) => {
            let mut buf = Vec::new();
            out.to_writer(&mut buf)
                .map_err(|e| PyValueError::new_err(format!("encode failed: {e}")))?;
            Ok(Some(to_pybytes(py, buf)))
        }
        Err(diff::Fallback) => Ok(None),
    }
}

#[pymodule]
fn _secantus_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add(
        "__doc__",
        "Rust core for SecantusDB (Phase 1: sortkey, query, update, \
         expressions, projection, diff).",
    )?;
    m.add_function(wrap_pyfunction!(sortkey_encode_value, m)?)?;
    m.add_function(wrap_pyfunction!(sortkey_encode_value_directed, m)?)?;
    m.add_function(wrap_pyfunction!(query_matches, m)?)?;
    m.add_function(wrap_pyfunction!(apply_update, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate, m)?)?;
    m.add_function(wrap_pyfunction!(apply_projection, m)?)?;
    m.add_function(wrap_pyfunction!(compute_update_description, m)?)?;
    Ok(())
}
