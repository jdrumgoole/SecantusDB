//! `_secantus_storage` — PyO3 bindings exposing SecantusDB's Rust storage layer
//! (`secantus-storage`, Phase 4) to Python.
//!
//! Like the `_secantus_core` operator engines, documents cross the boundary as
//! **BSON bytes** (`secantus.storage`'s "documents are opaque BSON blobs"
//! design), so the seam stays aligned and avoids per-field marshalling. `_id`
//! values are passed wrapped as `bson.encode({"v": <id>})` (the same one-key
//! envelope the sort-key seam uses).
//!
//! This extension links the vendored WiredTiger C library (via
//! `secantus-storage` → `secantus-wt`), so unlike `_secantus_core` it does not
//! build in maturin's plain manylinux container — shipping it across the wheel
//! matrix is Phase 4's go/no-go gate. For now it proves the binding + the
//! WiredTiger-linking extension build and import end-to-end.

// PyO3's #[pymethods] expansion inserts an identity `.into()` on PyResult return
// types that clippy flags as a useless conversion; it's a macro artifact, not
// our code (same suppression as secantus-core-py).
#![allow(clippy::useless_conversion)]

use bson::{Bson, Document};
use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use secantus_storage::{Storage, StorageError};

/// Map a storage error to a Python exception. `DuplicateId` becomes a
/// `KeyError` (the closest built-in to MongoDB's duplicate-key condition; the
/// command layer maps real E11000 codes); everything else is a `ValueError`.
fn to_pyerr(e: StorageError) -> PyErr {
    match e {
        StorageError::DuplicateId => PyKeyError::new_err("E11000 duplicate key error"),
        other => PyValueError::new_err(other.to_string()),
    }
}

/// Decode `bson.encode({"v": <id>})` and return the wrapped `_id` value.
fn unwrap_id(id_bytes: &[u8]) -> PyResult<Bson> {
    let doc: Document = bson::from_slice(id_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid id wrapper BSON: {e}")))?;
    doc.get("v")
        .cloned()
        .ok_or_else(|| PyValueError::new_err("id wrapper document missing key 'v'"))
}

fn bytes(py: Python<'_>, b: Vec<u8>) -> Py<PyBytes> {
    PyBytes::new_bound(py, &b).unbind()
}

/// A WiredTiger-backed SecantusDB storage handle (the Rust `Storage`).
#[pyclass(name = "RustStorage")]
struct RustStorage {
    inner: Storage,
}

#[pymethods]
impl RustStorage {
    /// Open (creating if needed) an on-disk database at `home`.
    #[new]
    fn new(home: &str) -> PyResult<Self> {
        Storage::open(home)
            .map(|inner| RustStorage { inner })
            .map_err(to_pyerr)
    }

    /// Open with an explicit WiredTiger config string.
    #[staticmethod]
    fn open_with_config(home: &str, config: &str) -> PyResult<Self> {
        Storage::open_with_config(home, config)
            .map(|inner| RustStorage { inner })
            .map_err(to_pyerr)
    }

    /// Insert one BSON-encoded document; returns its `id_key` bytes. Assigns an
    /// `ObjectId` `_id` when absent; a duplicate `_id` raises `KeyError`.
    fn insert_one(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        doc_bytes: &[u8],
    ) -> PyResult<Py<PyBytes>> {
        self.inner
            .insert_one(db, coll, doc_bytes)
            .map(|k| bytes(py, k))
            .map_err(to_pyerr)
    }

    /// Fetch a document by `_id` (wrapped as `{"v": id}`). Returns its BSON
    /// bytes or `None`.
    fn find_by_id(
        &self,
        py: Python<'_>,
        db: &str,
        coll: &str,
        id_bytes: &[u8],
    ) -> PyResult<Option<Py<PyBytes>>> {
        let id = unwrap_id(id_bytes)?;
        self.inner
            .find_by_id(db, coll, &id)
            .map(|opt| opt.map(|b| bytes(py, b)))
            .map_err(to_pyerr)
    }

    /// All documents of a collection in natural (`_id`) order, as BSON bytes.
    fn scan_collection(&self, py: Python<'_>, db: &str, coll: &str) -> PyResult<Vec<Py<PyBytes>>> {
        self.inner
            .scan_collection(db, coll)
            .map(|docs| docs.into_iter().map(|b| bytes(py, b)).collect())
            .map_err(to_pyerr)
    }

    /// Replace the document at `_id` with `new_doc_bytes` (whose `_id` is forced
    /// to the given id). Returns `False` if no such document.
    fn replace_by_id(
        &self,
        db: &str,
        coll: &str,
        id_bytes: &[u8],
        new_doc_bytes: &[u8],
    ) -> PyResult<bool> {
        let id = unwrap_id(id_bytes)?;
        self.inner
            .replace_by_id(db, coll, &id, new_doc_bytes)
            .map_err(to_pyerr)
    }

    /// Delete the document with the given `_id`. Returns `False` if absent.
    fn delete_by_id(&self, db: &str, coll: &str, id_bytes: &[u8]) -> PyResult<bool> {
        let id = unwrap_id(id_bytes)?;
        self.inner.delete_by_id(db, coll, &id).map_err(to_pyerr)
    }

    fn collection_exists(&self, db: &str, coll: &str) -> PyResult<bool> {
        self.inner.collection_exists(db, coll).map_err(to_pyerr)
    }

    fn list_collections(&self, db: &str) -> PyResult<Vec<String>> {
        self.inner.list_collections(db).map_err(to_pyerr)
    }
}

#[pymodule]
fn _secantus_storage(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add(
        "__doc__",
        "Rust storage layer for SecantusDB (Phase 4): WiredTiger-backed \
         collections/documents CRUD, behind the BSON byte seam.",
    )?;
    m.add_class::<RustStorage>()?;
    Ok(())
}
