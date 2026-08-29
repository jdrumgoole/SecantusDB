//! `_secantus_core` — the PyO3 bindings that expose the `secantus-core` engines
//! to Python.
//!
//! Everything crosses the Python/Rust boundary through the **fat byte seam**
//! (tasks/rust-rewrite-plan.md §3): values and documents travel as BSON bytes
//! (a one-key envelope like `{"v": <value>}` / `{"e": <expr>}` / `{"d": [docs]}`),
//! never as marshalled per-field Python objects. This keeps the seam aligned
//! with SecantusDB's "documents are opaque BSON blobs" design and avoids
//! per-field conversion costs.
//!
//! The pure operator engines themselves live in the sibling `secantus-core`
//! crate (no PyO3); this crate is only the byte-seam decode/encode + the
//! `#[pyfunction]` wrappers. Ported so far: the six leaf engines (`sortkey`,
//! `query.matches`, `update.apply_update`, `expressions.evaluate`,
//! `projection.apply_projection`, `diff.compute_update_description`) plus the
//! storage-independent aggregation pipeline (`apply_pipeline`). Each Python
//! module is a shim that delegates here when its component is enabled (via
//! `secantus.engine`) and otherwise runs the pure-Python path — the two
//! implementations are permanent and parity-pinned.
//!
//! **GIL discipline:** each `#[pyfunction]` decodes its BSON arguments while
//! holding the GIL (the byte slices borrow Python buffers), then runs the pure
//! Rust compute inside `Python::detach` (0.29 renamed `allow_threads`) so concurrent callers don't
//! serialise on it. See `benchmarks/` for the throughput characterisation (the
//! win is large single-threaded; multi-core scaling needs the per-doc seams
//! batched — coarse calls like `apply_pipeline` already benefit).

// PyO3 0.22's #[pyfunction] expansion inserts an identity `.into()` on the
// return type that clippy flags as a useless conversion; it's a macro artifact,
// not our code. Suppress at the crate level until we move to a PyO3 that fixed it.
#![allow(clippy::useless_conversion)]

use bson::{Bson, Document};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

use secantus_core::{aggregate, collation, diff, expressions, projection, query, sortkey, update};

/// Decode the one-key wrapper document and hand back the wrapped value.
fn unwrap_value(doc_bytes: &[u8]) -> PyResult<bson::Bson> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid BSON wrapper: {e}")))?;
    doc.get("v")
        .cloned()
        .ok_or_else(|| PyValueError::new_err("wrapper document missing key 'v'"))
}

fn to_pybytes(py: Python<'_>, bytes: Vec<u8>) -> Py<PyBytes> {
    PyBytes::new(py, &bytes).unbind()
}

/// Encode a document to BSON bytes inside a GIL-released closure (returns a
/// `String` error rather than a GIL-bound `PyErr` so it satisfies `Ungil`).
fn encode_doc(doc: &Document) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    doc.to_writer(&mut buf)
        .map_err(|e| format!("encode failed: {e}"))?;
    Ok(buf)
}

/// `sortkey.encode_value(value, collation=)` — `doc_bytes` is
/// `bson.encode({"v": value})`, `collation_bytes` the `{strength, caseLevel,
/// numericOrdering}` doc (or `{}`). Returns `None` to defer to Python (a value
/// the encoder doesn't handle, or a collation that needs non-ASCII / Unicode
/// normalisation).
#[pyfunction]
fn sortkey_encode_value(
    py: Python<'_>,
    doc_bytes: &[u8],
    collation_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let value = unwrap_value(doc_bytes)?;
    let coll = parse_collation(collation_bytes)?;
    let out = py.detach(|| sortkey::encode_value(&value, coll.as_ref()).ok());
    Ok(out.map(|b| to_pybytes(py, b)))
}

/// `sortkey.encode_value_directed(value, direction, collation=)`.
#[pyfunction]
fn sortkey_encode_value_directed(
    py: Python<'_>,
    doc_bytes: &[u8],
    direction: i32,
    collation_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let value = unwrap_value(doc_bytes)?;
    let coll = parse_collation(collation_bytes)?;
    let out = py.detach(|| sortkey::encode_value_directed(&value, direction, coll.as_ref()).ok());
    Ok(out.map(|b| to_pybytes(py, b)))
}

fn parse_collation(bytes: &[u8]) -> PyResult<Option<collation::Collation>> {
    let doc: Document = bson::from_slice(bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid collation BSON: {e}")))?;
    Ok(collation::parse(&doc))
}

/// `query.matches(doc, query)` over BSON bytes. Returns `None` to signal the
/// caller should fall back to the pure-Python matcher (the query uses a feature
/// not ported yet: collation, `$expr`, `$jsonSchema`, geo, regex, `$all`, …).
#[pyfunction]
fn query_matches(
    py: Python<'_>,
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
    let coll = parse_collation(collation_bytes)?;
    // Ok(b) -> Some(b) (a real result); Err(Fallback) -> None (defer to Python).
    // The match runs with the GIL released so concurrent callers parallelise.
    Ok(py.detach(|| query::matches(&doc, &query, &vars, coll.as_ref()).ok()))
}

/// `query.matches_raw` — the raw-BSON matcher, over the same BSON bytes. The
/// document bytes are matched WITHOUT being materialised into an owned
/// `Document` (only the filter's reached fields are decoded). Result must be
/// identical to [`query_matches`] for every input; the parity suite pins
/// `matches_raw == matches == pure Python`. Returns `None` to defer, same
/// contract as `query_matches`.
#[pyfunction]
fn query_matches_raw(
    py: Python<'_>,
    doc_bytes: &[u8],
    query_bytes: &[u8],
    vars_bytes: &[u8],
    collation_bytes: &[u8],
) -> PyResult<Option<bool>> {
    let query: Document = bson::from_slice(query_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid query BSON: {e}")))?;
    let vars: Document = bson::from_slice(vars_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid vars BSON: {e}")))?;
    let coll = parse_collation(collation_bytes)?;
    let raw = bson::RawDocument::from_bytes(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    Ok(py.detach(|| query::matches_raw(raw, &query, &vars, coll.as_ref()).ok()))
}

/// Batched `query.matches`: one call filters a whole candidate list under a
/// single GIL release, amortising the per-call seam + GIL handoff that makes
/// per-doc matching scale poorly under concurrency (see `benchmarks/`).
///
/// `docs_bytes` is `bson.encode({"d": [doc, ...]})`; the result is
/// `bson.encode({"m": [bool, ...]})`, one flag per input doc. Returns `None`
/// (whole-batch fallback) if any doc's match defers — the caller then runs the
/// pure-Python matcher per doc.
#[pyfunction]
fn query_matches_batch(
    py: Python<'_>,
    docs_bytes: &[u8],
    query_bytes: &[u8],
    vars_bytes: &[u8],
    collation_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let docs_wrap: Document = bson::from_slice(docs_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid docs BSON: {e}")))?;
    let query: Document = bson::from_slice(query_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid query BSON: {e}")))?;
    let vars: Document = bson::from_slice(vars_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid vars BSON: {e}")))?;
    let coll = parse_collation(collation_bytes)?;
    let Some(Bson::Array(arr)) = docs_wrap.get("d") else {
        return Err(PyValueError::new_err("docs wrapper missing array 'd'"));
    };
    let mut docs: Vec<Document> = Vec::with_capacity(arr.len());
    for d in arr {
        match d {
            Bson::Document(doc) => docs.push(doc.clone()),
            _ => return Ok(None), // non-document element -> defer the batch
        }
    }
    let out = py
        .detach(move || {
            let mut flags: Vec<Bson> = Vec::with_capacity(docs.len());
            for doc in &docs {
                match query::matches(doc, &query, &vars, coll.as_ref()) {
                    Ok(b) => flags.push(Bson::Boolean(b)),
                    Err(_) => return Ok(None), // any defer -> whole batch falls back
                }
            }
            let mut wrap = Document::new();
            wrap.insert("m".to_string(), Bson::Array(flags));
            encode_doc(&wrap).map(Some)
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
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
    let out = py
        .detach(|| match expressions::evaluate(&doc, expr, &vars) {
            Ok(value) => {
                let mut wrap = Document::new();
                wrap.insert("r".to_string(), value);
                encode_doc(&wrap).map(Some)
            }
            Err(expressions::Fallback) => Ok(None),
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
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
    let out = py
        .detach(|| match update::apply_update(&doc, &update, is_upsert) {
            Ok(new) => encode_doc(&new).map(Some),
            Err(update::Fallback) => Ok(None),
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}

/// `update.apply_update` with `arrayFilters` + positional support. `array_filters`
/// is a BSON doc `{"f": [<filter>, ...]}`; `positional_matches` is the `$`
/// resolution `{path: index}` (Python computes it via `find_positional_matches`).
/// Same `None`-fallback contract as [`apply_update`]. The parity-test vehicle for
/// the Rust server's positional/arrayFilters update path.
#[pyfunction]
fn apply_update_with(
    py: Python<'_>,
    doc_bytes: &[u8],
    update_bytes: &[u8],
    is_upsert: bool,
    array_filters_bytes: &[u8],
    positional_matches_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let update: Document = bson::from_slice(update_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid update BSON: {e}")))?;
    let af_wrap: Document = bson::from_slice(array_filters_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid arrayFilters BSON: {e}")))?;
    let array_filters: Vec<Document> = match af_wrap.get("f") {
        Some(bson::Bson::Array(a)) => a.iter().filter_map(|b| b.as_document().cloned()).collect(),
        _ => Vec::new(),
    };
    let pos: Document = bson::from_slice(positional_matches_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid positionalMatches BSON: {e}")))?;
    let out = py
        .detach(|| {
            match update::apply_update_with(&doc, &update, is_upsert, &array_filters, &pos) {
                Ok(new) => encode_doc(&new).map(Some),
                Err(update::Fallback) => Ok(None),
            }
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}

/// Batched `update.apply_update`: apply one update spec to a whole matched list
/// in one call (the multi-update hot path), one GIL release for all N. Mirrors
/// `query_matches_batch`'s seam: `{"d": [doc, ...]}` in, `{"d": [new, ...]}` out,
/// whole-batch fallback (`None`) if any doc defers.
#[pyfunction]
fn apply_update_batch(
    py: Python<'_>,
    docs_bytes: &[u8],
    update_bytes: &[u8],
    is_upsert: bool,
) -> PyResult<Option<Py<PyBytes>>> {
    let docs_wrap: Document = bson::from_slice(docs_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid docs BSON: {e}")))?;
    let update: Document = bson::from_slice(update_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid update BSON: {e}")))?;
    let Some(Bson::Array(arr)) = docs_wrap.get("d") else {
        return Err(PyValueError::new_err("docs wrapper missing array 'd'"));
    };
    let mut docs: Vec<Document> = Vec::with_capacity(arr.len());
    for d in arr {
        match d {
            Bson::Document(doc) => docs.push(doc.clone()),
            _ => return Ok(None),
        }
    }
    let out = py
        .detach(move || {
            let mut results: Vec<Bson> = Vec::with_capacity(docs.len());
            for doc in &docs {
                match update::apply_update(doc, &update, is_upsert) {
                    Ok(new) => results.push(Bson::Document(new)),
                    Err(update::Fallback) => return Ok(None),
                }
            }
            let mut wrap = Document::new();
            wrap.insert("d".to_string(), Bson::Array(results));
            encode_doc(&wrap).map(Some)
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}
/// projected document's bytes, or `None` to fall back to pure Python (mixed
/// inclusion/exclusion which Python raises on, nested-document specs, unusual
/// `$slice` args, or a `$elemMatch` sub-filter the matcher defers).
#[pyfunction]
#[pyo3(signature = (doc_bytes, spec_bytes, query_bytes=None))]
fn apply_projection(
    py: Python<'_>,
    doc_bytes: &[u8],
    spec_bytes: &[u8],
    query_bytes: Option<&[u8]>,
) -> PyResult<Option<Py<PyBytes>>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let spec: Document = bson::from_slice(spec_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid spec BSON: {e}")))?;
    let query = decode_optional_query(query_bytes)?;
    let out = py
        .detach(
            || match projection::apply_projection(&doc, &spec, query.as_ref()) {
                Ok(out) => encode_doc(&out).map(Some),
                Err(projection::Fallback) => Ok(None),
            },
        )
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}

/// `projection.apply_projection` raw-BSON fast path over the same bytes. Returns
/// `Some(projected bytes)` for the pure top-level inclusion shape it handles
/// (projecting straight off the raw document), or `None` to signal the caller
/// must run the full `apply_projection`. Result must be byte-identical to
/// `apply_projection` for every spec it claims; the parity suite pins it. The
/// GIL is held (the `RawDocument` borrows the Python buffer and the op is
/// small), unlike the batched projection which releases it.
#[pyfunction]
fn apply_projection_raw(
    py: Python<'_>,
    doc_bytes: &[u8],
    spec_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let spec: Document = bson::from_slice(spec_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid spec BSON: {e}")))?;
    let raw = bson::RawDocument::from_bytes(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    match projection::apply_projection_raw(raw, &spec) {
        Some(doc) => {
            let bytes = encode_doc(&doc).map_err(PyValueError::new_err)?;
            Ok(Some(to_pybytes(py, bytes)))
        }
        None => Ok(None),
    }
}

/// `secantus_core::referenced_top_level_fields` — the `$group` field-reference
/// pushdown. Given a `$group` spec, returns the sorted top-level field names the
/// group reads, or `None` when the group must run on fully-decoded documents
/// (whole-doc / computed-field / non-simple-accumulator shapes). The parity
/// suite pins the property that decoding only these fields yields byte-identical
/// `$group` output.
#[pyfunction]
fn group_referenced_fields(spec_bytes: &[u8]) -> PyResult<Option<Vec<String>>> {
    let spec: Document = bson::from_slice(spec_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid spec BSON: {e}")))?;
    Ok(
        secantus_core::referenced_top_level_fields(&Bson::Document(spec))
            .map(|s| s.into_iter().collect()),
    )
}

/// Decode an optional `query` filter (empty / absent -> `None`), used by the
/// projection bindings to resolve a positional `arr.$` projection.
fn decode_optional_query(query_bytes: Option<&[u8]>) -> PyResult<Option<Document>> {
    match query_bytes {
        Some(qb) if !qb.is_empty() => bson::from_slice(qb)
            .map(Some)
            .map_err(|e| PyValueError::new_err(format!("invalid query BSON: {e}"))),
        _ => Ok(None),
    }
}

/// Batched `projection.apply_projection`: project a whole result list in one
/// call (every `find` doc), one GIL release for all N. `{"d": [doc, ...]}` in,
/// `{"d": [projected, ...]}` out, whole-batch fallback (`None`) if any defers.
#[pyfunction]
#[pyo3(signature = (docs_bytes, spec_bytes, query_bytes=None))]
fn apply_projection_batch(
    py: Python<'_>,
    docs_bytes: &[u8],
    spec_bytes: &[u8],
    query_bytes: Option<&[u8]>,
) -> PyResult<Option<Py<PyBytes>>> {
    let docs_wrap: Document = bson::from_slice(docs_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid docs BSON: {e}")))?;
    let spec: Document = bson::from_slice(spec_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid spec BSON: {e}")))?;
    let query = decode_optional_query(query_bytes)?;
    let Some(Bson::Array(arr)) = docs_wrap.get("d") else {
        return Err(PyValueError::new_err("docs wrapper missing array 'd'"));
    };
    let mut docs: Vec<Document> = Vec::with_capacity(arr.len());
    for d in arr {
        match d {
            Bson::Document(doc) => docs.push(doc.clone()),
            _ => return Ok(None),
        }
    }
    let out = py
        .detach(move || {
            let mut results: Vec<Bson> = Vec::with_capacity(docs.len());
            for doc in &docs {
                match projection::apply_projection(doc, &spec, query.as_ref()) {
                    Ok(p) => results.push(Bson::Document(p)),
                    Err(projection::Fallback) => return Ok(None),
                }
            }
            let mut wrap = Document::new();
            wrap.insert("d".to_string(), Bson::Array(results));
            encode_doc(&wrap).map(Some)
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
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
    let out = py
        .detach(|| match diff::compute_update_description(&pre, &post) {
            Ok(out) => encode_doc(&out).map(Some),
            Err(diff::Fallback) => Ok(None),
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}

/// `diff.apply_update_description(doc, diff)` over BSON bytes — the inverse of
/// `compute_update_description` (rolls a pre-image forward by an oplog update's
/// `$v: 2` diff). Returns the post-image document's bytes, or `None` to fall back
/// to pure Python.
#[pyfunction]
fn apply_update_description(
    py: Python<'_>,
    doc_bytes: &[u8],
    diff_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let doc: Document = bson::from_slice(doc_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid doc BSON: {e}")))?;
    let diff: Document = bson::from_slice(diff_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid diff BSON: {e}")))?;
    let out = py
        .detach(|| match diff::apply_update_description(doc, &diff) {
            Ok(out) => encode_doc(&out).map(Some),
            Err(diff::Fallback) => Ok(None),
        })
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}

/// `aggregate.apply_pipeline(docs, pipeline, vars, collation)` over BSON bytes.
/// `docs_bytes` is `bson.encode({"d": [<doc>, ...]})` and `pipeline_bytes` is
/// `bson.encode({"p": [<stage>, ...]})`; the result is `{"d": [...]}` bytes, or
/// `None` to fall back to the pure-Python pipeline (any stage not ported, or an
/// inner expression that defers).
#[pyfunction]
fn apply_pipeline(
    py: Python<'_>,
    docs_bytes: &[u8],
    pipeline_bytes: &[u8],
    vars_bytes: &[u8],
    collation_bytes: &[u8],
) -> PyResult<Option<Py<PyBytes>>> {
    let docs_wrap: Document = bson::from_slice(docs_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid docs BSON: {e}")))?;
    let pipe_wrap: Document = bson::from_slice(pipeline_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid pipeline BSON: {e}")))?;
    let vars: Document = bson::from_slice(vars_bytes)
        .map_err(|e| PyValueError::new_err(format!("invalid vars BSON: {e}")))?;
    let coll = parse_collation(collation_bytes)?;

    let Some(Bson::Array(docs_arr)) = docs_wrap.get("d") else {
        return Err(PyValueError::new_err("docs wrapper missing array 'd'"));
    };
    let mut docs: Vec<Document> = Vec::with_capacity(docs_arr.len());
    for d in docs_arr {
        match d {
            Bson::Document(doc) => docs.push(doc.clone()),
            _ => return Ok(None), // non-document input element -> defer
        }
    }
    let Some(Bson::Array(pipeline)) = pipe_wrap.get("p") else {
        return Err(PyValueError::new_err("pipeline wrapper missing array 'p'"));
    };

    // The whole pipeline runs with the GIL released; only the BSON decode above
    // and the PyBytes build below need it.
    let out = py
        .detach(
            move || match aggregate::apply_pipeline(docs, pipeline, &vars, coll.as_ref()) {
                Ok(out) => {
                    let mut wrap = Document::new();
                    wrap.insert(
                        "d".to_string(),
                        Bson::Array(out.into_iter().map(Bson::Document).collect()),
                    );
                    encode_doc(&wrap).map(Some)
                }
                Err(aggregate::Fallback) => Ok(None),
            },
        )
        .map_err(PyValueError::new_err)?;
    Ok(out.map(|b| to_pybytes(py, b)))
}

#[pymodule]
fn _secantus_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add(
        "__doc__",
        "Rust core for SecantusDB: the six leaf engines (sortkey, query, update, \
         expressions, projection, diff) plus the storage-independent aggregation \
         pipeline, behind the BSON byte seam.",
    )?;
    m.add_function(wrap_pyfunction!(sortkey_encode_value, m)?)?;
    m.add_function(wrap_pyfunction!(sortkey_encode_value_directed, m)?)?;
    m.add_function(wrap_pyfunction!(query_matches, m)?)?;
    m.add_function(wrap_pyfunction!(query_matches_raw, m)?)?;
    m.add_function(wrap_pyfunction!(query_matches_batch, m)?)?;
    m.add_function(wrap_pyfunction!(apply_update, m)?)?;
    m.add_function(wrap_pyfunction!(apply_update_with, m)?)?;
    m.add_function(wrap_pyfunction!(apply_update_batch, m)?)?;
    m.add_function(wrap_pyfunction!(evaluate, m)?)?;
    m.add_function(wrap_pyfunction!(apply_projection, m)?)?;
    m.add_function(wrap_pyfunction!(apply_projection_raw, m)?)?;
    m.add_function(wrap_pyfunction!(apply_projection_batch, m)?)?;
    m.add_function(wrap_pyfunction!(compute_update_description, m)?)?;
    m.add_function(wrap_pyfunction!(apply_update_description, m)?)?;
    m.add_function(wrap_pyfunction!(apply_pipeline, m)?)?;
    m.add_function(wrap_pyfunction!(group_referenced_fields, m)?)?;
    Ok(())
}
