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

mod sortkey;

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

#[pymodule]
fn _secantus_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__doc__", "Rust core for SecantusDB (Phase 1: sortkey).")?;
    m.add_function(wrap_pyfunction!(sortkey_encode_value, m)?)?;
    m.add_function(wrap_pyfunction!(sortkey_encode_value_directed, m)?)?;
    Ok(())
}
