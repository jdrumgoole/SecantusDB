//! GIN and GiST indexes under SERIALIZABLE (RP-254): which of a table's
//! GIN and GiST indexes the page models (`gin_index.rs`, `gist_index.rs`)
//! cover, each one's model kept from its CREATE INDEX on, the inserts that
//! change it, the planner's choice of a scan through one, and the SIREAD
//! locks that scan takes.
//!
//! A GIN or GiST qual is never lowered to the storage's filter: it is in
//! the statement's residual, evaluated per row. A SELECT says what its
//! residual and LIMIT are while it reads ([`hint_select`]), and the SSI
//! scan of its table prices the GIN and GiST paths those quals open
//! beside the others (`gincostestimate`, `gistcostestimate`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use bson::{Bson, Document};
use pg_query::protobuf::node::Node as N;
use pgwire::error::PgWireResult;

use crate::gin_index::{self, Gin, Key as GinKey, KeyOps};
use crate::gist_index::{self, BoxF, Gist, Opclass, Query};

/// What a SELECT reading a table says of its scan: the residual its WHERE
/// left to evaluate per row, and how many rows a LIMIT stops it at.
#[derive(Clone)]
pub(crate) struct ScanHint {
    table: String,
    residual: Option<secantus_pgplan::ColumnExpr>,
    limit: Option<usize>,
}

thread_local! {
    static HINTS: RefCell<Vec<ScanHint>> = const { RefCell::new(Vec::new()) };
}

/// Pops its hint when the SELECT is done.
pub(crate) struct HintGuard;

impl Drop for HintGuard {
    fn drop(&mut self) {
        HINTS.with(|h| {
            h.borrow_mut().pop();
        });
    }
}

/// A SELECT over a stored table is about to read it: its residual, and its
/// LIMIT when nothing between the scan and the LIMIT reads every row (no
/// ORDER BY, DISTINCT, window or lock).
pub(crate) fn hint_select(sel: &secantus_pgplan::Select) -> HintGuard {
    let stops = sel.order.is_empty()
        && sel.distinct == secantus_pgplan::Distinct::None
        && sel.windows.is_empty()
        && !sel.with_ties
        && sel.lock.is_none()
        && sel.lock_clauses.is_empty();
    let limit = sel
        .limit
        .filter(|_| stops)
        .map(|l| usize::try_from(l.max(0) + sel.offset.max(0)).unwrap_or(usize::MAX));
    HINTS.with(|h| {
        h.borrow_mut().push(ScanHint {
            table: sel.table.clone(),
            residual: sel.residual.clone(),
            limit,
        })
    });
    HintGuard
}

fn hint_for(table: &str) -> Option<ScanHint> {
    HINTS.with(|h| h.borrow().iter().rev().find(|x| x.table == table).cloned())
}

/// A GIN index this models: one array column under `array_ops`.
#[derive(Debug, Clone)]
pub(crate) struct GinShape {
    pub(crate) oid: i64,
    pub(crate) name: String,
    pub(crate) field: String,
    /// The array's element type.
    pub(crate) elem: String,
    /// `fastupdate` (on unless `WITH` turned it off).
    pub(crate) fastupdate: bool,
    /// `gin_pending_list_limit`, kB, when `WITH` set it.
    pub(crate) pending_limit_kb: Option<usize>,
}

/// A GiST index this models: one `point` or `box` column under its
/// default opclass.
#[derive(Debug, Clone)]
pub(crate) struct GistShape {
    pub(crate) oid: i64,
    pub(crate) name: String,
    pub(crate) field: String,
    pub(crate) opclass: Opclass,
    pub(crate) fillfactor: u32,
}

#[derive(Debug, Clone)]
pub(crate) enum AmShape {
    Gin(GinShape),
    Gist(GistShape),
}

impl AmShape {
    fn oid(&self) -> i64 {
        match self {
            AmShape::Gin(s) => s.oid,
            AmShape::Gist(s) => s.oid,
        }
    }
    pub(crate) fn name(&self) -> &str {
        match self {
            AmShape::Gin(s) => &s.name,
            AmShape::Gist(s) => &s.name,
        }
    }
}

/// `array_ops` over an element type: its B-tree comparison, and the sizes
/// its keys take in an index tuple.
pub(crate) struct ElemOps {
    elem: String,
}

impl ElemOps {
    fn numeric(v: &Bson) -> Option<f64> {
        match v {
            Bson::Int32(i) => Some(f64::from(*i)),
            Bson::Int64(i) => Some(*i as f64),
            Bson::Double(d) => Some(*d),
            _ => None,
        }
    }

    /// The element types whose order and size the model reproduces.
    fn supported(elem: &str) -> bool {
        matches!(
            elem,
            "int2"
                | "int4"
                | "int8"
                | "smallint"
                | "integer"
                | "bigint"
                | "oid"
                | "float4"
                | "float8"
                | "real"
                | "double precision"
                | "bool"
                | "boolean"
                | "text"
                | "varchar"
                | "character varying"
        )
    }

    fn textual(&self) -> bool {
        matches!(self.elem.as_str(), "text" | "varchar" | "character varying")
    }
}

impl KeyOps for ElemOps {
    fn cmp(&self, a: &Bson, b: &Bson) -> std::cmp::Ordering {
        match (a, b) {
            (Bson::Int32(x), Bson::Int32(y)) => x.cmp(y),
            (Bson::Int64(x), Bson::Int64(y)) => x.cmp(y),
            (Bson::Boolean(x), Bson::Boolean(y)) => x.cmp(y),
            (Bson::String(x), Bson::String(y)) => x.as_bytes().cmp(y.as_bytes()),
            _ => match (Self::numeric(a), Self::numeric(b)) {
                // `btfloat8cmp`: NaN above everything.
                (Some(x), Some(y)) => match (x.is_nan(), y.is_nan()) {
                    (true, true) => std::cmp::Ordering::Equal,
                    (true, false) => std::cmp::Ordering::Greater,
                    (false, true) => std::cmp::Ordering::Less,
                    _ => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
                },
                _ => crate::compare_values(a, b),
            },
        }
    }

    fn key_size(&self, v: &Bson) -> usize {
        if self.textual() {
            let len = match v {
                Bson::String(s) => secantus_pgplan::server_encoding::bytes(s).len(),
                other => other.to_string().len(),
            };
            // A short varlena header when it fits one byte.
            let data = if len < 127 { 1 + len } else { 4 + len };
            return (8 + data + 7) & !7;
        }
        16
    }

    fn datum_size(&self, v: &Bson) -> Option<usize> {
        self.textual().then(|| match v {
            Bson::String(s) => 4 + secantus_pgplan::server_encoding::bytes(s).len(),
            other => 4 + other.to_string().len(),
        })
    }
}

/// A model of one index.
#[derive(Debug, Clone)]
enum Model {
    Gin(Gin),
    Gist(Gist),
}

type ModelKey = (usize, String, i64);

fn models() -> &'static Mutex<HashMap<ModelKey, Model>> {
    static M: OnceLock<Mutex<HashMap<ModelKey, Model>>> = OnceLock::new();
    M.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A GIN key list of a stored value (`ginExtractEntries`).
fn gin_keys(ops: &ElemOps, v: Option<&Bson>) -> Vec<GinKey> {
    match v {
        None | Some(Bson::Null) => gin_index::value_keys(ops, None),
        Some(Bson::Array(a)) => gin_index::value_keys(ops, Some(a)),
        Some(other) => gin_index::value_keys(ops, Some(std::slice::from_ref(other))),
    }
}

/// A GiST key of a stored value (`gist_point_compress`, or the box).
fn gist_key(v: Option<&Bson>) -> gist_index::Key {
    match secantus_pgplan::geom::from_bson(v?)? {
        secantus_pgplan::geom::Geo::Point(p) => Some(BoxF::point(p.x, p.y)),
        secantus_pgplan::geom::Geo::Box(h, l) => Some(BoxF {
            hx: h.x,
            hy: h.y,
            lx: l.x,
            ly: l.y,
        }),
        _ => None,
    }
}

/// One residual conjunct `column op constant` (the column on the left,
/// the operator commuted when it was on the right).
#[derive(Debug, Clone)]
struct OpQual {
    field: String,
    op: String,
    value: Bson,
}

/// A residual's top-level conjuncts as `column op constant`, or `None`
/// when any conjunct is something else.
fn residual_quals(residual: &secantus_pgplan::ColumnExpr) -> Option<Vec<OpQual>> {
    let secantus_pgplan::ColumnExpr::Row {
        expr,
        fields,
        params,
        ..
    } = residual
    else {
        return None;
    };
    let mut conjuncts = Vec::new();
    let mut stack = vec![expr.as_ref().clone()];
    while let Some(n) = stack.pop() {
        match n.node.as_ref() {
            Some(N::BoolExpr(b))
                if pg_query::protobuf::BoolExprType::try_from(b.boolop)
                    == Ok(pg_query::protobuf::BoolExprType::AndExpr) =>
            {
                stack.extend(b.args.iter().rev().cloned());
            }
            _ => conjuncts.push(n),
        }
    }
    let n_params = params.len();
    let column = |n: &pg_query::protobuf::Node| -> Option<usize> {
        match n.node.as_ref()? {
            N::ParamRef(p) if p.number as usize > n_params => {
                Some(p.number as usize - n_params - 1)
            }
            _ => None,
        }
    };
    let mentions_column = |n: &pg_query::protobuf::Node| -> bool {
        n.node.as_ref().is_some_and(|x| {
            x.nodes().iter().any(|(r, _, _, _)| {
                matches!(r, pg_query::NodeRef::ParamRef(p) if p.number as usize > n_params)
            })
        })
    };
    let mut out = Vec::new();
    for c in conjuncts {
        let Some(N::AExpr(e)) = c.node.as_ref() else {
            return None;
        };
        if pg_query::protobuf::AExprKind::try_from(e.kind)
            != Ok(pg_query::protobuf::AExprKind::AexprOp)
        {
            return None;
        }
        let [op] = e.name.as_slice() else {
            return None;
        };
        let Some(N::String(op)) = op.node.as_ref() else {
            return None;
        };
        let (l, r) = (e.lexpr.as_deref()?, e.rexpr.as_deref()?);
        let (col, other, op) = match (column(l), column(r)) {
            (Some(i), None) if !mentions_column(r) => (i, r, op.sval.clone()),
            (None, Some(i)) if !mentions_column(l) => (i, l, commute(&op.sval)?.to_string()),
            _ => return None,
        };
        let constant = secantus_pgplan::ColumnExpr::Row {
            expr: Box::new(other.clone()),
            fields: fields.clone(),
            params: params.clone(),
            result_type: String::new(),
            widths: vec![None; fields.len()],
            scratch: Default::default(),
        };
        let value = secantus_pgplan::apply_row_expr(&constant, &Document::new()).ok()?;
        out.push(OpQual {
            field: fields.get(col)?.1.clone(),
            op,
            value,
        });
    }
    Some(out)
}

/// An operator's commutator, for `constant op column`.
fn commute(op: &str) -> Option<&'static str> {
    Some(match op {
        "@>" => "<@",
        "<@" => "@>",
        "&&" => "&&",
        "<<" => ">>",
        ">>" => "<<",
        "<^" => ">^",
        ">^" => "<^",
        "<<|" => "|>>",
        "|>>" => "<<|",
        _ => return None,
    })
}

/// `restriction_selectivity` of a qual whose estimator ignores statistics
/// (`positionsel`, `areasel`, `contsel`), or `arraycontsel` on a column
/// with none; `None` for one that would read statistics.
fn qual_selectivity(q: &OpQual, column_type: &str, analyzed: bool) -> Option<f64> {
    let geo = matches!(column_type, "point" | "box");
    if geo {
        return match (column_type, q.op.as_str()) {
            ("point", "<<" | ">>" | "<^" | ">^" | "<<|" | "|>>") => Some(0.1),
            ("point", "<@") => Some(0.001),
            ("box", "<<" | ">>" | "&<" | "&>" | "<<|" | "|>>" | "&<|" | "|&>" | "<^" | ">^") => {
                Some(0.1)
            }
            ("box", "@>" | "<@") => Some(0.001),
            ("box", "&&") => Some(0.005),
            _ => None,
        };
    }
    if !column_type.ends_with("[]") || analyzed {
        return None;
    }
    let Bson::Array(elems) = &q.value else {
        return None;
    };
    // `mcelem_array_selec` with no most-common elements: each distinct
    // element `Min(DEFAULT_CONTAIN_SEL, minfreq / 2)`, minfreq a float4.
    let elem_selec = 0.005f64.min(f64::from(2.0f32 * 0.005f32 / 2.0));
    match q.op.as_str() {
        "@>" | "&&" => {
            if q.op == "@>" && elems.iter().any(|e| matches!(e, Bson::Null)) {
                return Some(0.0);
            }
            let mut vals: Vec<&Bson> = elems.iter().filter(|e| !matches!(e, Bson::Null)).collect();
            vals.sort_by(|a, b| crate::compare_values(a, b));
            vals.dedup_by(|a, b| crate::compare_values(a, b) == std::cmp::Ordering::Equal);
            let mut selec = if q.op == "@>" { 1.0 } else { 0.0 };
            for _ in vals {
                if q.op == "@>" {
                    selec *= elem_selec;
                } else {
                    selec = selec + elem_selec - selec * elem_selec;
                }
            }
            Some(selec.clamp(0.0, 1.0))
        }
        "<@" => Some(0.005),
        _ => None,
    }
}

/// A GIN strategy (`array_ops`): `&&` 1, `@>` 2, `<@` 3.
fn gin_strategy(op: &str) -> Option<u16> {
    Some(match op {
        "&&" => 1,
        "@>" => 2,
        "<@" => 3,
        _ => return None,
    })
}

/// A GiST strategy of `opclass` for `op` against `value`, and the query.
fn gist_query(opclass: Opclass, op: &str, value: &Bson) -> Option<Query> {
    let key = gist_key(Some(value))?;
    let is_box = matches!(
        secantus_pgplan::geom::from_bson(value),
        Some(secantus_pgplan::geom::Geo::Box(..))
    );
    let strategy = match (opclass, op, is_box) {
        (Opclass::Point, "<<", false) => 1,
        (Opclass::Point, ">>", false) => 5,
        (Opclass::Point, "<<|", false) => 10,
        (Opclass::Point, "|>>", false) => 11,
        (Opclass::Point, "<^", false) => 29,
        (Opclass::Point, ">^", false) => 30,
        (Opclass::Point, "<@", true) => 28,
        (Opclass::Box, "<<", true) => 1,
        (Opclass::Box, "&<", true) => 2,
        (Opclass::Box, "&&", true) => 3,
        (Opclass::Box, "&>", true) => 4,
        (Opclass::Box, ">>", true) => 5,
        (Opclass::Box, "@>", true) => 7,
        (Opclass::Box, "<@", true) => 8,
        (Opclass::Box, "&<|", true) => 9,
        (Opclass::Box, "<<|", true) => 10,
        (Opclass::Box, "|>>", true) => 11,
        (Opclass::Box, "|&>", true) => 12,
        _ => return None,
    };
    Some(Query {
        strategy,
        arg: key,
        arg_is_box: is_box && opclass == Opclass::Point,
    })
}

/// A GIN or GiST path the planner weighs for a scan, and what it is.
pub(crate) struct AmPath {
    pub(crate) shape: AmShape,
    /// The quals the index serves (positions in the residual's quals).
    quals: Vec<usize>,
    pub(crate) bitmap: (f64, f64),
    /// A plain index scan (GiST only).
    pub(crate) index: Option<(f64, f64)>,
}

/// The residual's quals priced for the planner: every conjunct as a
/// filter operator, their selectivity, and the GIN and GiST paths they
/// open.
pub(crate) struct AmPlan {
    quals: Vec<OpQual>,
    pub(crate) selectivity: f64,
    pub(crate) paths: Vec<AmPath>,
    pub(crate) limit: Option<usize>,
    residual: Option<secantus_pgplan::ColumnExpr>,
}

impl AmPlan {
    pub(crate) fn nquals(&self) -> usize {
        self.quals.len()
    }
}

impl crate::PgHandler {
    fn am_store(&self) -> usize {
        std::sync::Arc::as_ptr(&self.storage) as usize
    }

    /// `table`'s GIN and GiST indexes the models cover; one left out is
    /// never chosen for a SERIALIZABLE scan, which then locks the table.
    pub(crate) fn am_shapes(&self, table: &str) -> Vec<AmShape> {
        type Memo = HashMap<(usize, String), (u64, std::sync::Arc<HashMap<String, Vec<AmShape>>>)>;
        static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
        if table.starts_with("__") {
            return Vec::new();
        }
        let version = crate::catalog_cache()
            .version
            .load(std::sync::atomic::Ordering::SeqCst);
        let mid_ddl = self
            .catalog_statements
            .load(std::sync::atomic::Ordering::Relaxed)
            > 0
            || self.block_wrote_catalog();
        let key = (self.am_store(), self.db().to_string());
        let memo = MEMO.get_or_init(|| Mutex::new(HashMap::new()));
        if !mid_ddl {
            if let Some((v, m)) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
                if *v == version {
                    return m.get(table).cloned().unwrap_or_default();
                }
            }
        }
        let mut by_table: HashMap<String, Vec<AmShape>> = HashMap::new();
        let mut stored_of: HashMap<String, Vec<Document>> = HashMap::new();
        for ix in self.index_relations() {
            if ix.primary || ix.exclusion || ix.keys.len() != 1 || ix.keys[0] <= 0 {
                continue;
            }
            let def = &ix.table;
            let stored = stored_of.entry(def.name.clone()).or_insert_with(|| {
                self.storage
                    .list_indexes(self.db(), &def.name)
                    .unwrap_or_default()
            });
            let Some(d) = stored
                .iter()
                .find(|d| d.get_str("name") == Ok(ix.name.as_str()))
            else {
                continue;
            };
            let opt = |k: &str| {
                d.get(k).cloned().or_else(|| {
                    d.get_document("options")
                        .ok()
                        .and_then(|o| o.get(k).cloned())
                })
            };
            let method = opt("sqlMethod");
            let method = method.as_ref().and_then(Bson::as_str).unwrap_or("btree");
            if !matches!(method, "gin" | "gist") || opt("sqlPredicate").is_some() {
                continue;
            }
            // The column's default opclass only.
            let explicit = match opt("sqlOpclasses") {
                Some(Bson::Array(a)) => a.iter().any(|v| crate::bson_i64(v).unwrap_or(0) != 0),
                _ => false,
            };
            if explicit || opt("sqlExpressions").is_some() {
                continue;
            }
            let Some(field) = d
                .get_document("key")
                .ok()
                .and_then(|k| k.keys().next().cloned())
            else {
                continue;
            };
            let Some(col) = def.columns.iter().find(|c| c.field() == field) else {
                continue;
            };
            let reloptions: Vec<String> = match opt("sqlReloptions") {
                Some(Bson::Array(a)) => a
                    .iter()
                    .filter_map(Bson::as_str)
                    .map(str::to_string)
                    .collect(),
                _ => Vec::new(),
            };
            let reloption = |name: &str| {
                reloptions.iter().find_map(|o| {
                    o.strip_prefix(&format!("{name}="))
                        .map(|v| v.trim().to_ascii_lowercase())
                })
            };
            let shape = if method == "gin" {
                let Some(elem) = col.pg_type.strip_suffix("[]") else {
                    continue;
                };
                if !ElemOps::supported(elem) || col.extra.contains_key("collation") {
                    continue;
                }
                AmShape::Gin(GinShape {
                    oid: ix.oid,
                    name: ix.name.clone(),
                    field: col.field(),
                    elem: elem.to_string(),
                    fastupdate: !matches!(
                        reloption("fastupdate").as_deref(),
                        Some("off" | "false" | "no" | "0")
                    ),
                    pending_limit_kb: reloption("gin_pending_list_limit")
                        .and_then(|v| v.parse().ok()),
                })
            } else {
                let opclass = match col.pg_type.as_str() {
                    "point" => Opclass::Point,
                    "box" => Opclass::Box,
                    _ => continue,
                };
                AmShape::Gist(GistShape {
                    oid: ix.oid,
                    name: ix.name.clone(),
                    field: col.field(),
                    opclass,
                    fillfactor: reloption("fillfactor")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(90),
                })
            };
            by_table.entry(def.name.clone()).or_default().push(shape);
        }
        let m = std::sync::Arc::new(by_table);
        if !mid_ddl {
            memo.lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (version, std::sync::Arc::clone(&m)));
        }
        m.get(table).cloned().unwrap_or_default()
    }

    /// A kB setting of this session, or its default.
    fn kb_setting(&self, name: &str, default: usize) -> usize {
        let v = self
            .settings
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned();
        let Some(v) = v else {
            return default;
        };
        let v = v.trim().to_ascii_lowercase();
        let (num, mult) = if let Some(n) = v.strip_suffix("gb") {
            (n, 1024 * 1024)
        } else if let Some(n) = v.strip_suffix("mb") {
            (n, 1024)
        } else if let Some(n) = v.strip_suffix("kb") {
            (n, 1)
        } else {
            (v.as_str(), 1)
        };
        num.trim().parse::<usize>().map_or(default, |n| n * mult)
    }

    /// `table`'s rows in heap order, each with its `ctid`.
    fn am_rows(&self, table: &str) -> Vec<(u64, Document)> {
        let mut rows: Vec<(u64, Document)> = self
            .table_docs(table)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|d| Some((crate::heap::ctid_of(&d)? as u64, d)))
            .collect();
        rows.sort_by_key(|(c, _)| *c);
        rows
    }

    /// CREATE INDEX's build of `shape` over `table` as it is now.
    fn am_build(&self, table: &str, shape: &AmShape) -> Model {
        let rows = self.am_rows(table);
        match shape {
            AmShape::Gin(s) => {
                let ops = ElemOps {
                    elem: s.elem.clone(),
                };
                let keyed: Vec<(u64, Vec<GinKey>)> = rows
                    .iter()
                    .map(|(c, d)| (*c, gin_keys(&ops, d.get(&s.field))))
                    .collect();
                Model::Gin(Gin::build(
                    &ops,
                    &keyed,
                    self.kb_setting("maintenance_work_mem", 65536),
                ))
            }
            AmShape::Gist(s) => {
                let keyed: Vec<(u64, gist_index::Key)> = rows
                    .iter()
                    .map(|(c, d)| (*c, gist_key(d.get(&s.field))))
                    .collect();
                Model::Gist(Gist::build(s.opclass, &keyed, s.fillfactor))
            }
        }
    }

    /// `shape`'s model, built over the rows now if it has none (one first
    /// met after a restart).
    fn with_am_model<T>(&self, table: &str, shape: &AmShape, f: impl FnOnce(&mut Model) -> T) -> T {
        let key = (self.am_store(), self.db().to_string(), shape.oid());
        let exists = models()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&key);
        let fresh = (!exists).then(|| self.am_build(table, shape));
        let mut m = models().lock().unwrap_or_else(|e| e.into_inner());
        let model = m.entry(key).or_insert_with(|| fresh.expect("built"));
        f(model)
    }

    /// CREATE INDEX, REINDEX or a rewrite: the models of `table`'s GIN
    /// and GiST indexes (those `only` names, or every one) built afresh.
    pub(crate) fn am_built(&self, table: &str, only: Option<&[String]>) {
        for shape in self.am_shapes(table) {
            if only.is_some_and(|names| !names.iter().any(|n| n == shape.name())) {
                continue;
            }
            let model = self.am_build(table, &shape);
            models()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.am_store(), self.db().to_string(), shape.oid()), model);
        }
    }

    /// TRUNCATE: each index empty again.
    pub(crate) fn am_truncated(&self, table: &str) {
        for shape in self.am_shapes(table) {
            let model = match &shape {
                AmShape::Gin(_) => Model::Gin(Gin::empty()),
                AmShape::Gist(_) => Model::Gist(Gist::empty()),
            };
            models()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert((self.am_store(), self.db().to_string(), shape.oid()), model);
        }
    }

    /// VACUUM's index pass: the dead TIDs leave each model.
    pub(crate) fn am_bulk_delete(&self, table: &str, dead: &std::collections::HashSet<i64>) {
        let work_mem = self.kb_setting("maintenance_work_mem", 65536);
        for shape in self.am_shapes(table) {
            let ops = match &shape {
                AmShape::Gin(s) => Some(ElemOps {
                    elem: s.elem.clone(),
                }),
                AmShape::Gist(_) => None,
            };
            self.with_am_model(table, &shape, |m| match (m, &ops) {
                (Model::Gin(g), Some(ops)) => {
                    g.vacuum(ops, &|t| dead.contains(&(t as i64)), work_mem)
                }
                (Model::Gist(g), _) => g.vacuum(&|t| dead.contains(&(t as i64))),
                _ => {}
            });
        }
    }

    /// New tuple versions of `table` that are not heap-only, into each GIN
    /// and GiST index (`gininsert`, `gistinsert`): the pages an insert
    /// changes checked for conflicts in, a split's locks copied on.
    pub(crate) fn am_insert_versions(
        &self,
        table: &str,
        versions: &[(&Document, Option<&Document>, i64)],
    ) -> PgWireResult<()> {
        if versions.is_empty() {
            return Ok(());
        }
        // A table with no GIN or GiST index (most) needs no setting read.
        let shapes = self.am_shapes(table);
        if shapes.is_empty() {
            return Ok(());
        }
        let work_mem = self.kb_setting("work_mem", 4096);
        let pending_limit = self.kb_setting("gin_pending_list_limit", 4096);
        for shape in shapes {
            let oid = shape.oid();
            for (doc, _, ctid) in versions {
                let tid = *ctid as u64;
                let events: Vec<(bool, u32, u32)> = match &shape {
                    AmShape::Gin(s) => {
                        let ops = ElemOps {
                            elem: s.elem.clone(),
                        };
                        let keys = gin_keys(&ops, doc.get(&s.field));
                        self.with_am_model(table, &shape, |m| match m {
                            Model::Gin(g) => g
                                .insert(
                                    &ops,
                                    tid,
                                    &keys,
                                    s.fastupdate,
                                    s.pending_limit_kb.unwrap_or(pending_limit),
                                    work_mem,
                                )
                                .into_iter()
                                .map(|e| match e {
                                    gin_index::Event::ConflictIn(b) => (true, b, 0),
                                    gin_index::Event::Split(a, b) => (false, a, b),
                                })
                                .collect(),
                            Model::Gist(_) => Vec::new(),
                        })
                    }
                    AmShape::Gist(s) => {
                        let key = gist_key(doc.get(&s.field));
                        self.with_am_model(table, &shape, |m| match m {
                            Model::Gist(g) => g
                                .insert(tid, key)
                                .into_iter()
                                .map(|e| match e {
                                    gist_index::Event::ConflictIn(b) => (true, b, 0),
                                    gist_index::Event::Split(a, b) => (false, a, b),
                                })
                                .collect(),
                            Model::Gin(_) => Vec::new(),
                        })
                    }
                };
                for (conflict, a, b) in events {
                    if conflict {
                        self.ssi_index_insert(oid, a)?;
                    } else {
                        self.ssi_page_split(oid, a, b);
                    }
                }
            }
        }
        Ok(())
    }

    /// The residual of the SELECT reading `table` priced for the planner,
    /// with the GIN and GiST paths its quals open; `None` when there is no
    /// residual, or one this cannot price.
    pub(crate) fn am_plan(
        &self,
        table: &str,
        pt: &crate::PlannerTable,
        rel: &secantus_pgplan::costsize::Rel,
        filter_quals: &[(String, secantus_pgplan::costsize::Qual)],
        rows_before: f64,
    ) -> Option<AmPlan> {
        use secantus_pgplan::costsize;
        let hint = hint_for(table)?;
        let residual = hint.residual.clone()?;
        let quals = residual_quals(&residual)?;
        let column_type = |f: &str| {
            pt.def
                .columns
                .iter()
                .find(|c| c.field() == f)
                .map(|c| c.pg_type.clone())
        };
        let analyzed = |f: &str| pt.analyzed.as_ref().is_some_and(|a| a.contains_key(f));
        let mut sels = Vec::with_capacity(quals.len());
        for q in &quals {
            let ty = column_type(&q.field)?;
            sels.push(qual_selectivity(q, &ty, analyzed(&q.field))?);
        }
        let selectivity: f64 = sels.iter().product();
        let rows = (rows_before * selectivity).round().max(1.0);
        // Every qual is a filter operator on every path; the index serves
        // its own besides.
        let mut all_quals: Vec<(String, costsize::Qual)> = filter_quals.to_vec();
        all_quals.extend(
            quals
                .iter()
                .map(|_| (String::new(), costsize::Qual::Filter(1.0))),
        );
        let mut paths = Vec::new();
        for shape in self.am_shapes(table) {
            let (field, served): (&str, Vec<usize>) = match &shape {
                AmShape::Gin(s) => (
                    &s.field,
                    quals
                        .iter()
                        .enumerate()
                        .filter(|(_, q)| q.field == s.field && gin_strategy(&q.op).is_some())
                        .map(|(i, _)| i)
                        .collect(),
                ),
                AmShape::Gist(s) => (
                    &s.field,
                    quals
                        .iter()
                        .enumerate()
                        .filter(|(_, q)| {
                            q.field == s.field && gist_query(s.opclass, &q.op, &q.value).is_some()
                        })
                        .map(|(i, _)| i)
                        .collect(),
                ),
            };
            let _ = field;
            if served.is_empty() {
                continue;
            }
            let sel: f64 = served.iter().map(|i| sels[*i]).product();
            let pages = self.with_am_model(table, &shape, |m| match m {
                Model::Gin(g) => g.pages(),
                Model::Gist(g) => g.pages(),
            });
            let (bitmap, index) = match &shape {
                AmShape::Gin(_) => {
                    let stats = self.with_am_model(table, &shape, |m| match m {
                        Model::Gin(g) => g.stats(),
                        Model::Gist(_) => gin_index::Stats::default(),
                    });
                    let mut counts = costsize::GinCounts::default();
                    let mut normal = false;
                    for i in &served {
                        let q = &quals[*i];
                        let n = match &q.value {
                            Bson::Array(a) => a.len(),
                            _ => 1,
                        };
                        match (q.op.as_str(), n) {
                            ("@>", 0) => counts.full_scan = true,
                            ("<@", _) => {
                                counts.exact_entries += n as f64 + 1.0;
                                counts.search_entries += n as f64 + 1.0;
                                normal = true;
                            }
                            (_, 0) => counts.void = true,
                            _ => {
                                counts.exact_entries += n as f64;
                                counts.search_entries += n as f64;
                                normal = true;
                            }
                        }
                    }
                    if normal {
                        counts.full_scan = false;
                    }
                    let gstats = costsize::GinStats {
                        pending_pages: f64::from(stats.n_pending_pages),
                        total_pages: f64::from(stats.n_total_pages),
                        entry_pages: f64::from(stats.n_entry_pages),
                        data_pages: f64::from(stats.n_data_pages),
                        entries: stats.n_entries as f64,
                    };
                    let (_, total) = costsize::gin_index_cost(
                        f64::from(pages),
                        rel.tuples,
                        gstats,
                        counts,
                        sel,
                        served.len(),
                    );
                    (
                        costsize::bitmap_heap_over(rel, total, sel, rows, &all_quals),
                        None,
                    )
                }
                AmShape::Gist(_) => {
                    let (startup, total) =
                        costsize::gist_index_cost(f64::from(pages), rel.tuples, sel, served.len());
                    let mut others: Vec<(String, costsize::Qual)> = filter_quals.to_vec();
                    others.extend(
                        (0..quals.len())
                            .filter(|i| !served.contains(i))
                            .map(|_| (String::new(), costsize::Qual::Filter(1.0))),
                    );
                    (
                        costsize::bitmap_heap_over(rel, total, sel, rows, &all_quals),
                        Some(costsize::index_scan_over(rel, startup, total, sel, &others)),
                    )
                }
            };
            paths.push(AmPath {
                shape,
                quals: served,
                bitmap,
                index,
            });
        }
        Some(AmPlan {
            quals,
            selectivity,
            paths,
            limit: hint.limit,
            residual: Some(residual),
        })
    }

    /// The SERIALIZABLE scan of `table` through `path`: the index pages
    /// its scan reads, locked; then each heap tuple the scan fetches,
    /// visible ones locked, in TID order a heap page at a time for a
    /// bitmap scan and in the index's order for a plain one -- each to the
    /// row that completes the LIMIT.
    pub(crate) fn ssi_am_scan(
        &self,
        table: &str,
        rel: i64,
        filter: &Document,
        plan: &AmPlan,
        path: &AmPath,
        bitmap: bool,
    ) -> PgWireResult<()> {
        let pid = self.backend_pid.load(std::sync::atomic::Ordering::Relaxed);
        let db = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            self.db().hash(&mut h);
            h.finish()
        };
        let oid = path.shape.oid();
        // The scan's pages, and the TIDs it returns in its order.
        let (pages, tids): (Vec<u32>, Vec<u64>) = match &path.shape {
            AmShape::Gin(s) => {
                let ops = ElemOps {
                    elem: s.elem.clone(),
                };
                self.with_am_model(table, &path.shape, |m| {
                    let Model::Gin(g) = m else {
                        return (Vec::new(), Vec::new());
                    };
                    gin_scan(g, &ops, &plan.quals, &path.quals)
                })
            }
            AmShape::Gist(s) => {
                let queries: Vec<Query> = path
                    .quals
                    .iter()
                    .filter_map(|i| {
                        let q = &plan.quals[*i];
                        gist_query(s.opclass, &q.op, &q.value)
                    })
                    .collect();
                let steps = self.with_am_model(table, &path.shape, |m| match m {
                    Model::Gist(g) => g.scan(s.opclass, &queries),
                    Model::Gin(_) => Vec::new(),
                });
                let mut pages = Vec::new();
                let mut tids = Vec::new();
                for st in steps {
                    match st {
                        gist_index::Step::Page(b) => pages.push(b),
                        gist_index::Step::Tuple(t) => tids.push(t),
                    }
                }
                if bitmap {
                    tids.sort_unstable();
                    tids.dedup();
                }
                (pages, tids)
            }
        };
        // The visible rows, by ctid: what each TID fetches.
        let rows = self
            .storage
            .find_matching(self.db(), table, &Document::new())
            .map_err(|e| Self::storage_err("could not read", e))?;
        let mut visible: HashMap<u64, (Option<u64>, Document)> = HashMap::new();
        for b in &rows {
            let Ok(d) = crate::decode_doc(b) else {
                continue;
            };
            let Some(c) = crate::heap::ctid_of(&d) else {
                continue;
            };
            let xmin = d
                .get("__sys_xmin")
                .and_then(crate::bson_i64)
                .map(|x| x as u64);
            visible.insert(c as u64, (xmin, d));
        }
        let empty = Document::new();
        let passes = |d: &Document| {
            secantus_core::query::matches(d, filter, &empty, None).unwrap_or(false)
                && plan.residual.as_ref().is_none_or(|r| {
                    secantus_pgplan::apply_row_expr(r, d).is_ok_and(|v| v == Bson::Boolean(true))
                })
        };
        // The TIDs fetched before the LIMIT has its rows.
        let mut fetched: Vec<u64> = Vec::new();
        let mut returned = 0usize;
        let want = plan.limit.unwrap_or(usize::MAX);
        if bitmap {
            let mut i = 0;
            while i < tids.len() && returned < want {
                let blk = tids[i] >> 16;
                while i < tids.len() && tids[i] >> 16 == blk {
                    fetched.push(tids[i]);
                    if visible.get(&tids[i]).is_some_and(|(_, d)| passes(d)) {
                        returned += 1;
                    }
                    i += 1;
                }
            }
        } else {
            for t in &tids {
                if returned >= want {
                    break;
                }
                fetched.push(*t);
                if visible.get(t).is_some_and(|(_, d)| passes(d)) {
                    returned += 1;
                }
            }
        }
        // A plain scan stops reading index pages where the LIMIT stopped
        // it; the pages up to the last TID fetched are the ones it read.
        let pages: Vec<u32> = if bitmap || plan.limit.is_none() {
            pages
        } else {
            self.with_am_model(table, &path.shape, |m| match (m, &path.shape) {
                (Model::Gist(g), AmShape::Gist(s)) => {
                    let queries: Vec<Query> = path
                        .quals
                        .iter()
                        .filter_map(|i| {
                            gist_query(s.opclass, &plan.quals[*i].op, &plan.quals[*i].value)
                        })
                        .collect();
                    let mut out = Vec::new();
                    let mut n = 0usize;
                    for st in g.scan(s.opclass, &queries) {
                        match st {
                            gist_index::Step::Page(b) => {
                                if n >= fetched.len() && !fetched.is_empty() {
                                    break;
                                }
                                out.push(b);
                            }
                            gist_index::Step::Tuple(_) => n += 1,
                        }
                    }
                    out
                }
                _ => pages.clone(),
            })
        };
        let fetched_set: std::collections::HashSet<u64> = fetched.iter().copied().collect();
        let examined = |c: i64, _: Option<&Document>| fetched_set.contains(&(c as u64));
        let locks: Vec<(i64, Option<u64>)> = fetched
            .iter()
            .filter_map(|t| visible.get(t).map(|(x, _)| (*t as i64, *x)))
            .collect();
        self.ssi(|s| {
            for b in &pages {
                s.lock_page(pid, db, oid, *b);
            }
            let any = !locks.is_empty() || s.has_versions(db, rel, &examined);
            s.scan_conflicts_out(pid, db, rel, &examined, any)?;
            for (ctid, xmin) in &locks {
                s.lock_tuple(pid, db, rel, *ctid, *xmin);
            }
            Ok(())
        })
        .map_err(crate::serializable::failure)
    }
}

/// A GIN bitmap scan for the quals at `served`: the pages it locks, and
/// the TIDs its bitmap holds, sorted (`ginNewScanKey`'s entries, each
/// qual's consistent function over them).
fn gin_scan(g: &Gin, ops: &ElemOps, quals: &[OpQual], served: &[usize]) -> (Vec<u32>, Vec<u64>) {
    let mut entries: Vec<gin_index::ScanEntry> = Vec::new();
    let add = |k: GinKey, entries: &mut Vec<gin_index::ScanEntry>| {
        if !entries.iter().any(|e| {
            e.key.cat == k.cat
                && (k.cat != gin_index::Cat::Norm || ops.cmp(&e.key.val, &k.val).is_eq())
        }) {
            entries.push(gin_index::ScanEntry { key: k });
        }
    };
    let mut result: Option<std::collections::BTreeSet<u64>> = None;
    let mut full: Vec<usize> = Vec::new();
    let mut normal = false;
    for i in served {
        let q = &quals[*i];
        let elems: Vec<Bson> = match &q.value {
            Bson::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let keys: Vec<GinKey> = elems
            .iter()
            .map(|e| match e {
                Bson::Null => GinKey::cat(gin_index::Cat::NullKey),
                v => GinKey::norm(v.clone()),
            })
            .collect();
        for k in &keys {
            add(k.clone(), &mut entries);
        }
        let tid_sets: Vec<std::collections::BTreeSet<u64>> = keys
            .iter()
            .map(|k| g.key_tids(ops, k).into_iter().collect())
            .collect();
        let has_null = keys.iter().any(|k| k.cat != gin_index::Cat::Norm);
        let set: std::collections::BTreeSet<u64> = match q.op.as_str() {
            "@>" if keys.is_empty() => {
                full.push(*i);
                continue;
            }
            "@>" if has_null => std::collections::BTreeSet::new(),
            "@>" => {
                normal = true;
                let mut it = tid_sets.into_iter();
                let first = it.next().unwrap_or_default();
                it.fold(first, |acc, s| acc.intersection(&s).copied().collect())
            }
            "&&" => {
                normal = true;
                tid_sets.into_iter().flatten().collect()
            }
            _ => {
                // `<@`: any key, or an empty array (`INCLUDE_EMPTY`).
                normal = true;
                let empty = GinKey::cat(gin_index::Cat::EmptyItem);
                add(empty.clone(), &mut entries);
                let mut s: std::collections::BTreeSet<u64> =
                    tid_sets.into_iter().flatten().collect();
                s.extend(g.key_tids(ops, &empty));
                s
            }
        };
        result = Some(match result {
            None => set,
            Some(r) => r.intersection(&set).copied().collect(),
        });
    }
    if !full.is_empty() && !normal {
        // `GIN_SEARCH_MODE_ALL` with no other key on the column: the hidden
        // `GIN_CAT_EMPTY_QUERY` entry scans every non-NULL item.
        add(GinKey::cat(gin_index::Cat::EmptyQuery), &mut entries);
        let mut all = std::collections::BTreeSet::new();
        for t in g.all_tids(ops) {
            all.insert(t);
        }
        result = Some(match result {
            None => all,
            Some(r) => r.intersection(&all).copied().collect(),
        });
    }
    let pages = g.scan_locks(ops, &entries);
    (pages, result.unwrap_or_default().into_iter().collect())
}
