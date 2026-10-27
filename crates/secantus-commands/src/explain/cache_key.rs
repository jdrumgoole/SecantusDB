//! mongod's plan cache key, as 8.2.11 builds it for its classic plan cache.
//!
//! Transcribed from `canonical_query_encoder.cpp` (`encodeClassic`,
//! `encodeForPlanCacheCommand`), `plan_cache_key_factory.cpp` (the
//! indexability discriminators) and `plan_cache_key_info.h` (the two
//! hashes), and checked against mongod 8.2.11's `explain` and
//! `$planCacheStats` (2026-10-27: every shape `tools/probes/plan_cache.py`
//! sends hashes alike).
//!
//! The QUERY SHAPE is the match expression with its values left out (each
//! node's type and path, pre-order), then the sort, the fields the
//! projection needs, the collation and the distinct key, each after a `|`,
//! then `f` (no `apiStrict`) and `c` (a plan run by the classic engine) or
//! `s` (planned by the classic planner and run in SBE).
//! `planCacheShapeHash` (and its old name `queryHash`) is mongod's
//! `computeHash` of the shape: the first eight bytes of MurmurHash3
//! x64-128, seed 0, cut to 32 bits. The KEY adds, per leaf, whether each
//! index on its path could serve it (`<1>`, `<10>`, ...), and for a
//! partial index whether the query implies its filter (`(1)`);
//! `planCacheKey` hashes it the same way, combined (boost's
//! `hash_combine`) with the hash of the query settings, none here.

use bson::{Bson, Document};
use secantus_core::collation::Collation;
use std::collections::BTreeSet;

use super::node::IndexInfo;

/// MurmurHash3 x64-128 (Austin Appleby's reference), the first 64 bits.
fn murmur3_x64_128_h1(data: &[u8], seed: u32) -> u64 {
    const C1: u64 = 0x87c3_7b91_1142_53d5;
    const C2: u64 = 0x4cf5_ad43_2745_937f;
    let fmix = |mut k: u64| {
        k ^= k >> 33;
        k = k.wrapping_mul(0xff51_afd7_ed55_8ccd);
        k ^= k >> 33;
        k = k.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
        k ^= k >> 33;
        k
    };
    let mut h1 = u64::from(seed);
    let mut h2 = u64::from(seed);
    let blocks = data.len() / 16;
    for i in 0..blocks {
        let at = i * 16;
        let mut k1 = u64::from_le_bytes(data[at..at + 8].try_into().expect("8 bytes"));
        let mut k2 = u64::from_le_bytes(data[at + 8..at + 16].try_into().expect("8 bytes"));
        k1 = k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        h1 ^= k1;
        h1 = h1
            .rotate_left(27)
            .wrapping_add(h2)
            .wrapping_mul(5)
            .wrapping_add(0x52dc_e729);
        k2 = k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        h2 ^= k2;
        h2 = h2
            .rotate_left(31)
            .wrapping_add(h1)
            .wrapping_mul(5)
            .wrapping_add(0x3849_5ab5);
    }
    let tail = &data[blocks * 16..];
    let mut k1: u64 = 0;
    let mut k2: u64 = 0;
    for (i, &b) in tail.iter().enumerate().rev() {
        if i >= 8 {
            k2 ^= u64::from(b) << ((i - 8) * 8);
        } else {
            k1 ^= u64::from(b) << (i * 8);
        }
    }
    if tail.len() > 8 {
        k2 = k2.wrapping_mul(C2).rotate_left(33).wrapping_mul(C1);
        h2 ^= k2;
    }
    if !tail.is_empty() {
        k1 = k1.wrapping_mul(C1).rotate_left(31).wrapping_mul(C2);
        h1 ^= k1;
    }
    let len = data.len() as u64;
    h1 ^= len;
    h2 ^= len;
    h1 = h1.wrapping_add(h2);
    h2 = h2.wrapping_add(h1);
    h1 = fmix(h1);
    h2 = fmix(h2);
    h1.wrapping_add(h2)
}

/// `canonical_query_encoder::computeHash`: `murmur3<8>` with seed 0, as a
/// `uint32_t`.
pub(crate) fn compute_hash(key: &str) -> u32 {
    murmur3_x64_128_h1(key.as_bytes(), 0) as u32
}

/// boost's `hash_combine` for a 64-bit `size_t` (boost 1.81 and later, as
/// mongod 8.2 vendors it).
fn hash_combine(seed: u64, value: u64) -> u64 {
    let mut x = seed.wrapping_add(0x9e37_79b9).wrapping_add(value);
    const M: u64 = 0x0e98_46af_9b1a_615d;
    x ^= x >> 32;
    x = x.wrapping_mul(M);
    x ^= x >> 32;
    x = x.wrapping_mul(M);
    x ^= x >> 28;
    x
}

/// `query_settings::hash` of empty settings: no framework, no index hints
/// (each an empty `boost::optional`, hashed as `0x12345678`), not rejected.
fn empty_settings_hash() -> u64 {
    let h = hash_combine(0, 0x1234_5678);
    let h = hash_combine(h, 0x1234_5678);
    hash_combine(h, 0)
}

/// `PlanCacheKeyInfo::planCacheKeyHash`.
pub(crate) fn key_hash(key: &str) -> u32 {
    hash_combine(u64::from(compute_hash(key)), empty_settings_hash()) as u32
}

/// A hash as `explain` and `$planCacheStats` print it.
pub(crate) fn hex(hash: u32) -> String {
    format!("{hash:08X}")
}

const SPECIAL: &[char] = &['[', ']', ',', '|', '-', '/', '?', ':', '\\'];

/// `encodeUserString`: a path or field name, its delimiters escaped.
fn user_string(s: &str, out: &mut String) {
    for c in s.chars() {
        if SPECIAL.contains(&c) {
            out.push('\\');
        }
        out.push(c);
    }
}

/// The flags of some regexes as the key carries them: the valid ones
/// (`imsx`), sorted, once each, between slashes.
fn regex_flags<'a>(options: impl Iterator<Item = &'a str>, out: &mut String) {
    let flags: BTreeSet<char> = options
        .flat_map(str::chars)
        .filter(|c| "imsx".contains(*c))
        .collect();
    if !flags.is_empty() {
        out.push('/');
        for f in flags {
            user_string(&f.to_string(), out);
        }
        out.push('/');
    }
}

/// The leaf operators, as `encodeMatchType` names their nodes.
fn leaf_code(op: &str) -> Option<&'static str> {
    Some(match op {
        "$eq" => "eq",
        "$lt" => "lt",
        "$lte" => "le",
        "$gt" => "gt",
        "$gte" => "ge",
        "$in" => "in",
        "$regex" => "re",
        "$exists" => "ex",
        "$type" => "ty",
        "$mod" => "mo",
        "$size" => "sz",
        "$bitsAllSet" => "ls",
        "$bitsAllClear" => "lc",
        "$bitsAnySet" => "ys",
        "$bitsAnyClear" => "yc",
        "$_internalExprEq" => "eeq",
        "$_internalExprGt" => "egt",
        "$_internalExprGte" => "ege",
        "$_internalExprLt" => "elt",
        "$_internalExprLte" => "ele",
        "$geoWithin" | "$within" | "$geoIntersects" => "go",
        "$near" | "$nearSphere" | "$geoNear" => "gn",
        _ => return None,
    })
}

/// One node of the parsed match expression, as the encoders walk it.
#[derive(Clone, Debug)]
pub(crate) enum Expr {
    /// `$and`, `$or`, `$nor`.
    Logical(&'static str, Vec<Expr>),
    /// `$not` over its child.
    Not(Box<Expr>),
    /// `$elemMatch` of an object (one child, paths inside it relative).
    ElemObject(String, Box<Expr>),
    /// `$elemMatch` of values (children with no path).
    ElemValue(String, Vec<Expr>),
    /// A predicate on a path: the operator and its operand.
    Leaf {
        path: String,
        op: String,
        value: Bson,
        /// `$regex`'s `$options`.
        options: String,
    },
    /// A node with no path and no children: `$expr`, `$where`, `$text`,
    /// `$alwaysFalse`, `$alwaysTrue`.
    Bare(&'static str),
}

fn elem_value_ops(d: &Document) -> bool {
    !d.is_empty()
        && d.keys().all(|k| {
            k.starts_with('$')
                && !matches!(k.as_str(), "$and" | "$or" | "$nor" | "$expr" | "$where")
        })
}

/// The operators under one path, as nodes.
fn path_ops(path: &str, ops: &Document) -> Option<Vec<Expr>> {
    let mut out = Vec::new();
    for (op, value) in ops {
        match op.as_str() {
            "$options" => {}
            "$not" => {
                let inner = value.as_document()?;
                let child = and_of(path_ops(path, inner)?);
                out.push(Expr::Not(Box::new(child)));
            }
            "$elemMatch" => {
                let inner = value.as_document()?;
                if elem_value_ops(inner) {
                    out.push(Expr::ElemValue(path.to_string(), path_ops("", inner)?));
                } else {
                    out.push(Expr::ElemObject(path.to_string(), Box::new(parse(inner)?)));
                }
            }
            "$regex" => {
                let (pattern_flags, value) = match value {
                    Bson::RegularExpression(r) => (r.options.clone(), value.clone()),
                    other => (String::new(), other.clone()),
                };
                let mut options = ops.get_str("$options").unwrap_or_default().to_string();
                options.push_str(&pattern_flags);
                out.push(Expr::Leaf {
                    path: path.to_string(),
                    op: op.clone(),
                    value,
                    options,
                });
            }
            other => {
                leaf_code(other)?;
                out.push(Expr::Leaf {
                    path: path.to_string(),
                    op: other.to_string(),
                    value: value.clone(),
                    options: String::new(),
                });
            }
        }
    }
    Some(out)
}

fn and_of(mut nodes: Vec<Expr>) -> Expr {
    if nodes.len() == 1 {
        nodes.pop().expect("one")
    } else {
        Expr::Logical("an", nodes)
    }
}

/// The parsed match expression (`parsed_query`'s output) as nodes. `None`:
/// a form this does not encode.
pub(crate) fn parse(q: &Document) -> Option<Expr> {
    let mut nodes = Vec::new();
    for (key, value) in q {
        let node = match key.as_str() {
            "$and" | "$or" | "$nor" => {
                let code = match key.as_str() {
                    "$and" => "an",
                    "$or" => "or",
                    _ => "nr",
                };
                let children = value
                    .as_array()?
                    .iter()
                    .map(|c| c.as_document().and_then(parse))
                    .collect::<Option<Vec<_>>>()?;
                Expr::Logical(code, children)
            }
            "$expr" => Expr::Bare("xp"),
            "$where" => Expr::Bare("wh"),
            "$text" => Expr::Bare("te"),
            "$alwaysFalse" => Expr::Bare("af"),
            "$alwaysTrue" => Expr::Bare("at"),
            k if k.starts_with('$') => return None,
            path => match value {
                Bson::Document(ops) if ops.keys().next().is_some_and(|k| k.starts_with('$')) => {
                    and_of(path_ops(path, ops)?)
                }
                other => Expr::Leaf {
                    path: path.to_string(),
                    op: "$eq".into(),
                    value: other.clone(),
                    options: String::new(),
                },
            },
        };
        nodes.push(node);
    }
    if nodes.is_empty() {
        return Some(Expr::Logical("an", vec![]));
    }
    Some(and_of(nodes))
}

/// The geometry type and CRS a geo predicate's operand names, as
/// `encodeGeoMatchExpression` and `encodeGeoNearMatchExpression` write them.
fn geo_tail(op: &str, value: &Bson, out: &mut String) {
    let doc = value.as_document();
    if op == "$near" || op == "$nearSphere" || op == "$geoNear" {
        out.push_str(if op == "$nearSphere" { "ns" } else { "nr" });
        let geojson = match value {
            Bson::Document(d) => d.contains_key("$geometry") || d.contains_key("type"),
            _ => false,
        };
        out.push_str(if geojson || op == "$nearSphere" {
            "sp"
        } else {
            "fl"
        });
        return;
    }
    out.push_str(if op == "$geoIntersects" { "in" } else { "wi" });
    let (kind, crs) = match doc.and_then(|d| d.iter().next()) {
        Some((k, v)) if k == "$geometry" => {
            let g = v.as_document();
            let t = g.and_then(|g| g.get_str("type").ok()).unwrap_or("");
            let strict = g
                .and_then(|g| g.get_document("crs").ok())
                .and_then(|c| c.get_document("properties").ok())
                .and_then(|p| p.get_str("name").ok())
                .is_some_and(|n| n.contains("strictwinding"));
            let kind = match t {
                "Point" => "pt",
                "LineString" => "ln",
                "Polygon" => "pl",
                "MultiPoint" => "mp",
                "MultiLineString" => "ml",
                "MultiPolygon" => "my",
                "GeometryCollection" => "gc",
                _ => "",
            };
            (kind, if strict { "ss" } else { "sp" })
        }
        Some((k, _)) if k == "$box" => ("bx", "fl"),
        Some((k, _)) if k == "$center" => ("cc", "fl"),
        Some((k, _)) if k == "$centerSphere" => ("cc", "sp"),
        Some((k, _)) if k == "$polygon" => ("pl", "fl"),
        _ => ("", "fl"),
    };
    out.push_str(kind);
    out.push_str(crs);
}

/// `encodeKeyForMatch`: pre-order, each node's type and path, children in
/// `[...]` separated by `,`.
pub(crate) fn encode_match(e: &Expr, out: &mut String) {
    match e {
        Expr::Logical(code, children) => {
            out.push_str(code);
            children_of(children, out);
        }
        Expr::Not(child) => {
            out.push_str("nt");
            if let Expr::Leaf { op, value, .. } = &**child {
                if matches!(op.as_str(), "$eq" | "$gte" | "$lte") && *value == Bson::Null {
                    out.push_str("not_eq_null");
                }
            }
            children_of(std::slice::from_ref(&**child), out);
        }
        Expr::ElemObject(path, child) => {
            out.push_str("eo");
            user_string(path, out);
            children_of(std::slice::from_ref(&**child), out);
        }
        Expr::ElemValue(path, children) => {
            out.push_str("ev");
            user_string(path, out);
            children_of(children, out);
        }
        Expr::Leaf {
            path,
            op,
            value,
            options,
        } => {
            out.push_str(leaf_code(op).unwrap_or("eq"));
            user_string(path, out);
            match op.as_str() {
                "$regex" => regex_flags(std::iter::once(options.as_str()), out),
                "$in" => {
                    let regexes: Vec<&str> = value
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| match v {
                            Bson::RegularExpression(r) => Some(r.options.as_str()),
                            _ => None,
                        })
                        .collect();
                    let any = value
                        .as_array()
                        .is_some_and(|a| a.iter().any(|v| matches!(v, Bson::RegularExpression(_))));
                    if any {
                        user_string("_re", out);
                        regex_flags(regexes.into_iter(), out);
                    }
                }
                "$geoWithin" | "$within" | "$geoIntersects" | "$near" | "$nearSphere"
                | "$geoNear" => geo_tail(op, value, out),
                "$gt" if matches!(value, Bson::MinKey) => out.push_str("min"),
                "$lt" if matches!(value, Bson::MaxKey) => out.push_str("max"),
                _ => {}
            }
        }
        Expr::Bare(code) => out.push_str(code),
    }
}

fn children_of(children: &[Expr], out: &mut String) {
    if children.is_empty() {
        return;
    }
    out.push('[');
    for (i, c) in children.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        encode_match(c, out);
    }
    out.push(']');
}

/// `encodeKeyForSort`: after its `|`, `a` or `d` (or `t` for a text score)
/// and the field, comma separated.
fn encode_sort(sort: Option<&Document>, out: &mut String) {
    out.push('|');
    let Some(sort) = sort else { return };
    for (i, (field, dir)) in sort.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let meta = matches!(dir, Bson::Document(d) if d.get_str("$meta") == Ok("textScore"));
        let ascending = crate::util::as_i64(dir) == Some(1)
            || matches!(dir, Bson::Double(d) if (*d as i32) == 1);
        out.push(if meta {
            't'
        } else if ascending {
            'a'
        } else {
            'd'
        });
        user_string(field, out);
    }
}

/// The fields a projection needs (`projection_ast`'s required fields, in
/// order), or `None` when it needs the whole document: no projection, an
/// exclusion, or an expression over the document itself.
pub(crate) fn required_fields(projection: Option<&Document>) -> Option<BTreeSet<String>> {
    let projection = projection.filter(|p| !p.is_empty())?;
    let mut fields = BTreeSet::new();
    let mut id = true;
    let mut inclusion = false;
    for (field, value) in projection {
        if field == "_id" {
            match value {
                Bson::Boolean(false) => id = false,
                Bson::Int32(0) | Bson::Int64(0) => id = false,
                Bson::Double(d) if *d == 0.0 => id = false,
                _ => {}
            }
            continue;
        }
        match value {
            Bson::Boolean(false) | Bson::Int32(0) | Bson::Int64(0) => return None,
            Bson::Double(d) if *d == 0.0 => return None,
            Bson::Boolean(true) | Bson::Int32(_) | Bson::Int64(_) | Bson::Double(_) => {
                inclusion = true;
                fields.insert(field.trim_end_matches(".$").to_string());
            }
            Bson::Document(d) if d.contains_key("$slice") && d.len() == 1 => {
                // A `$slice` alone is an exclusion projection's.
                fields.insert(field.clone());
            }
            Bson::Document(d) if d.contains_key("$elemMatch") || d.contains_key("$meta") => {
                if d.contains_key("$elemMatch") {
                    inclusion = true;
                    fields.insert(field.clone());
                }
            }
            other => {
                inclusion = true;
                let mut paths = Vec::new();
                super::hoist::expr_paths(other, &mut paths)?;
                for p in paths {
                    if p.is_empty() || p == "$$ROOT" || p == "$$CURRENT" {
                        return None;
                    }
                    if p == "_id" || p.starts_with("_id.") {
                        id = true;
                    }
                    fields.insert(p);
                }
            }
        }
    }
    if !inclusion {
        return None;
    }
    if id {
        fields.insert("_id".into());
    }
    Some(fields)
}

fn encode_projection(fields: Option<&BTreeSet<String>>, out: &mut String) {
    out.push('|');
    let Some(fields) = fields else { return };
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push('-');
        }
        user_string(f, out);
    }
}

/// `encodeCollation`: after its `|`, the collator's spec, each enum as one
/// digit.
fn encode_collation(collation: Option<&Collation>, out: &mut String) {
    out.push('|');
    let Some(c) = collation else { return };
    let spec = c.spec();
    let flag = |k: &str| {
        if spec.get_bool(k).unwrap_or(false) {
            '1'
        } else {
            '0'
        }
    };
    out.push_str(spec.get_str("locale").unwrap_or(""));
    out.push(flag("caseLevel"));
    out.push(match spec.get_str("caseFirst").unwrap_or("off") {
        "upper" => '0',
        "lower" => '1',
        _ => '2',
    });
    let strength =
        crate::util::as_i64(spec.get("strength").unwrap_or(&Bson::Int32(3))).unwrap_or(3);
    out.push(char::from(b'0' + strength.clamp(0, 9) as u8));
    out.push(flag("numericOrdering"));
    out.push(if spec.get_str("alternate") == Ok("shifted") {
        '1'
    } else {
        '0'
    });
    out.push(if spec.get_str("maxVariable") == Ok("space") {
        '1'
    } else {
        '0'
    });
    out.push(flag("normalization"));
    out.push(flag("backwards"));
}

/// What a query brings to its key beside the match expression.
pub(crate) struct Parts<'a> {
    pub sort: Option<&'a Document>,
    pub projection: Option<&'a Document>,
    pub collation: Option<&'a Collation>,
    /// `distinct`'s key.
    pub distinct: Option<&'a str>,
    /// Planned by the classic planner and run in SBE.
    pub sbe: bool,
}

/// `encodeClassic`: the query shape.
pub(crate) fn shape(e: &Expr, p: &Parts) -> String {
    let mut out = String::new();
    encode_match(e, &mut out);
    encode_sort(p.sort, &mut out);
    encode_projection(required_fields(p.projection).as_ref(), &mut out);
    encode_collation(p.collation, &mut out);
    // `encodeKeyForDistinct`: the key, then whether the scan runs backward
    // (never, for a `distinct` command).
    out.push('|');
    if let Some(key) = p.distinct {
        user_string(key, &mut out);
        out.push('0');
    }
    out.push('f');
    out.push(if p.sbe { 's' } else { 'c' });
    out
}

/// `encodeForPlanCacheCommand`: the shape `planCacheClear` names, the
/// collation only when the query gave one.
pub(crate) fn command_shape(
    e: &Expr,
    sort: Option<&Document>,
    projection: Option<&Document>,
    collation: Option<&Collation>,
) -> String {
    let mut out = String::new();
    encode_match(e, &mut out);
    encode_sort(sort, &mut out);
    encode_projection(required_fields(projection).as_ref(), &mut out);
    match collation {
        Some(_) => encode_collation(collation, &mut out),
        None => out.push('|'),
    }
    out
}

/// A string, an object or an array: a value a collation compares.
fn collatable(v: &Bson) -> bool {
    matches!(
        v,
        Bson::String(_) | Bson::Symbol(_) | Bson::Document(_) | Bson::Array(_)
    )
}

const COMPARISONS: &[&str] = &[
    "$eq",
    "$lt",
    "$lte",
    "$gt",
    "$gte",
    "$_internalExprEq",
    "$_internalExprGt",
    "$_internalExprGte",
    "$_internalExprLt",
    "$_internalExprLte",
];

/// `getCollatedIndexDiscriminator`: whether a node compares strings under
/// another collation than the index's.
fn collation_fits(e: &Expr, index: &IndexInfo, query: Option<&Collation>) -> bool {
    let same = match (index.collation.as_ref(), query) {
        (None, None) => true,
        (Some(a), Some(b)) => a.same(b),
        _ => false,
    };
    match e {
        Expr::Leaf { op, value, .. } if COMPARISONS.contains(&op.as_str()) => {
            same || !collatable(value)
        }
        Expr::Leaf { op, value, .. } if op == "$in" => {
            same || !value.as_array().is_some_and(|a| {
                a.iter()
                    .any(|v| !matches!(v, Bson::RegularExpression(_)) && collatable(v))
            })
        }
        _ => true,
    }
}

/// `nodeIsSupportedBySparseIndex`, outside an `$elemMatch`.
fn sparse_fits(e: &Expr) -> bool {
    match e {
        Expr::Leaf { op, value, .. } if matches!(op.as_str(), "$eq" | "$gte" | "$lte") => {
            *value != Bson::Null
        }
        Expr::Leaf { op, value, .. } if op == "$in" => {
            !value.as_array().is_some_and(|a| a.contains(&Bson::Null))
        }
        Expr::Not(child) => {
            matches!(&**child, Expr::Leaf { op, value, .. } if op == "$eq" && *value == Bson::Null)
        }
        _ => true,
    }
}

/// `nodeIsSupportedByWildcardIndex`.
fn wildcard_fits(e: &Expr) -> bool {
    match e {
        Expr::Leaf { op, value, .. } if COMPARISONS[..5].contains(&op.as_str()) => match value {
            Bson::Document(d) => d.is_empty() && (op == "$eq" || op == "$lte"),
            Bson::Array(a) => a.is_empty() && op == "$eq",
            _ => true,
        },
        Expr::Leaf { op, value, .. } if op == "$in" => !value.as_array().is_some_and(|a| {
            a.iter().any(|v| match v {
                Bson::Array(x) => !x.is_empty(),
                Bson::Document(x) => !x.is_empty(),
                _ => false,
            })
        }),
        _ => true,
    }
}

/// `logicalNodeMayBeSupportedByAnIndex`: not a `$not` of a comparison to
/// an array.
fn not_may_use_index(child: &Expr) -> bool {
    match child {
        Expr::Leaf { op, value, .. } if COMPARISONS[..5].contains(&op.as_str()) => {
            !matches!(value, Bson::Array(_))
        }
        Expr::Leaf { op, value, .. } if op == "$in" => !value
            .as_array()
            .is_some_and(|a| a.iter().any(|v| matches!(v, Bson::Array(_)))),
        _ => true,
    }
}

/// A wildcard index's keyed paths: `$**` (everything but `_id`) or
/// `a.$**` (`a` and below).
fn wildcard_covers(index: &IndexInfo, path: &str) -> bool {
    index.key.keys().any(|k| {
        if k == "$**" {
            path != "_id" && !path.starts_with("_id.")
        } else if let Some(prefix) = k.strip_suffix(".$**") {
            path == prefix || path.starts_with(&format!("{prefix}."))
        } else {
            false
        }
    })
}

/// The indexes mongod discriminates by: the planner's catalog.
pub(crate) struct Indexability<'a> {
    pub catalog: &'a [std::rc::Rc<IndexInfo>],
    pub collation: Option<&'a Collation>,
    /// Whether the whole query implies a partial index's filter.
    pub implies: &'a dyn Fn(&Document) -> bool,
}

impl Indexability<'_> {
    fn path_discriminators(&self, e: &Expr, path: &str, out: &mut String) {
        let mut bits = String::new();
        for ix in self.catalog {
            if wildcard_covers(ix, path) {
                continue;
            }
            if !ix.key.keys().any(|k| k == path) {
                continue;
            }
            let fits = (!ix.sparse || sparse_fits(e)) && collation_fits(e, ix, self.collation);
            bits.push(if fits { '1' } else { '0' });
        }
        for ix in self.catalog.iter().filter(|ix| wildcard_covers(ix, path)) {
            let fits = wildcard_fits(e) && sparse_fits(e) && collation_fits(e, ix, self.collation);
            bits.push(if fits { '1' } else { '0' });
        }
        if !bits.is_empty() {
            out.push('<');
            out.push_str(&bits);
            out.push('>');
        }
    }

    fn walk(&self, e: &Expr, parent: &str, out: &mut String) {
        let own = match e {
            Expr::Leaf { path, .. } | Expr::ElemObject(path, _) | Expr::ElemValue(path, _) => {
                Some(path.as_str())
            }
            _ => None,
        };
        let path = match own.filter(|p| !p.is_empty()) {
            Some(p) if parent.is_empty() => p.to_string(),
            Some(p) => format!("{parent}.{p}"),
            None => parent.to_string(),
        };
        match e {
            Expr::Not(child) => {
                out.push('<');
                out.push(if not_may_use_index(child) { '1' } else { '0' });
                out.push('>');
            }
            Expr::Logical(..) => {}
            _ if !path.is_empty() => self.path_discriminators(e, &path, out),
            _ => {}
        }
        match e {
            Expr::Logical(_, children) | Expr::ElemValue(_, children) => {
                for c in children {
                    self.walk(c, &path, out);
                }
            }
            Expr::Not(child) | Expr::ElemObject(_, child) => self.walk(child, &path, out),
            _ => {}
        }
    }

    /// `encodeIndexability`: the partial indexes' global discriminators,
    /// then each node's.
    pub(crate) fn encode(&self, e: &Expr) -> String {
        let mut out = String::new();
        let partial: Vec<&std::rc::Rc<IndexInfo>> = self
            .catalog
            .iter()
            .filter(|ix| ix.partial.is_some())
            .collect();
        if !partial.is_empty() {
            out.push('(');
            for ix in partial {
                let implied = ix.partial.as_ref().is_some_and(|f| (self.implies)(f));
                out.push(if implied { '1' } else { '0' });
            }
            out.push(')');
        }
        self.walk(e, "", &mut out);
        out
    }
}

/// One query's place in the cache.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CacheKey {
    /// The shape followed by the indexability discriminators.
    pub key: String,
    pub shape_hash: u32,
    pub key_hash: u32,
}

/// The key for `parsed` (`parsed_query`'s output) with `parts`, over the
/// indexes `ix`. `None`: a match expression this does not encode.
pub(crate) fn key_for(parsed: &Document, parts: &Parts, ix: &Indexability) -> Option<CacheKey> {
    let e = parse(parsed)?;
    let shape = shape(&e, parts);
    let shape_hash = compute_hash(&shape);
    let mut key = shape;
    key.push_str(&ix.encode(&e));
    Some(CacheKey {
        key_hash: key_hash(&key),
        key,
        shape_hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bson::doc;

    #[test]
    fn hashes_are_mongods() {
        // mongod 8.2.11, 2026-10-27: `find({a: 1, b: 3})` over `a_1`, `b_1`.
        assert_eq!(hex(compute_hash("an[eqa,eqb]||||fc")), "BBC007A6");
        assert_eq!(hex(key_hash("an[eqa,eqb]||||fc<1><1>")), "51C56FDD");
        assert_eq!(hex(compute_hash("eqa||||fc")), "B521CD2E");
        assert_eq!(hex(compute_hash("an||||fc")), "4E45B701");
        assert_eq!(hex(compute_hash("eqa||_id-a||fc")), "72FD18C0");
        assert_eq!(hex(compute_hash("ina_re/i/||||fc")), "B8C39B8F");
        assert_eq!(hex(compute_hash("res/im/||||fc")), "4F486929");
    }

    #[test]
    fn murmur_every_tail_length() {
        // The first 64 bits of MurmurHash3 x64-128, seed 0, as the reference
        // implementation (Python's `mmh3.hash128`) gives them.
        let s = "0123456789abcdefghijklmnopqrstuv";
        for (n, want) in [
            (0, 0x0),
            (1, 0x2ac9_debe_d546_a380),
            (8, 0x8236_039b_7387_354d),
            (9, 0x4c1e_8751_9fe7_38ba),
            (15, 0xa62d_d5f6_c0bf_2351),
            (16, 0x4be0_6d94_cf4a_d1a7),
            (17, 0x8e32_612d_aa45_f9de),
            (31, 0xb828_780c_1a6e_0542),
            (32, 0x2f28_5cce_c0ed_f1f7),
        ] {
            assert_eq!(murmur3_x64_128_h1(&s.as_bytes()[..n], 0), want, "{n} bytes");
        }
    }

    #[test]
    fn shapes_of_parsed_queries() {
        let none = Parts {
            sort: None,
            projection: None,
            collation: None,
            distinct: None,
            sbe: false,
        };
        let shape_of = |q: Document| shape(&parse(&q).unwrap(), &none);
        assert_eq!(
            shape_of(doc! {"$and": [{"a": {"$eq": 1}}, {"b": {"$eq": 3}}]}),
            "an[eqa,eqb]||||fc"
        );
        assert_eq!(
            shape_of(doc! {"a": {"$not": {"$eq": Bson::Null}}}),
            "ntnot_eq_null[eqa]||||fc"
        );
        assert_eq!(
            shape_of(doc! {"t": {"$elemMatch": {"$lt": 5, "$gt": 1}}}),
            "evt[lt,gt]||||fc"
        );
        assert_eq!(
            shape_of(doc! {"t": {"$elemMatch": {"$and": [{"x": {"$eq": 1}}, {"y": {"$gt": 2}}]}}}),
            "eot[an[eqx,gty]]||||fc"
        );
        assert_eq!(shape_of(doc! {"a|b": {"$eq": 1}}), "eqa\\|b||||fc");
        assert_eq!(shape_of(doc! {"a": {"$gt": Bson::MinKey}}), "gtamin||||fc");
        let sorted = Parts {
            sort: Some(&doc! {"b": -1, "c": 1}),
            ..none
        };
        assert_eq!(
            shape(&parse(&doc! {"a": {"$eq": 1}}).unwrap(), &sorted),
            "eqa|db,ac|||fc"
        );
    }

    #[test]
    fn projection_fields() {
        let f = |p: Document| required_fields(Some(&p)).map(|s| s.into_iter().collect::<Vec<_>>());
        assert_eq!(
            f(doc! {"a": 1}),
            Some(vec!["_id".to_string(), "a".to_string()])
        );
        assert_eq!(f(doc! {"a": 1, "_id": 0}), Some(vec!["a".to_string()]));
        assert_eq!(f(doc! {"a": 0}), None);
        assert_eq!(
            f(doc! {"x": "$a"}),
            Some(vec!["_id".to_string(), "a".to_string()])
        );
    }
}
