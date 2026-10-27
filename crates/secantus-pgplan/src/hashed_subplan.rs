//! A hashed SubPlan: `(a, b, ...) IN (SELECT x, y, ... )` over an
//! uncorrelated subquery, answered by one hash probe a row
//! (`nodeSubplan.c`: `buildSubPlanHash`, `ExecHashSubPlan`).
//!
//! The subquery's rows are resolved while the statement is planned, and the
//! row comparison used to become an OR over them of AND-ed equalities
//! (`row_subquery_comparison`), evaluated in full for every outer row:
//! `information_schema.element_types` compared each of its rows with the
//! 11,734 rows of `data_type_privileges` (RP-131).
//!
//! PostgreSQL hashes such a SubPlan when `subplan_is_hashable` says so: an
//! `ANY` sublink, every comparison a hashable equality operator, and the
//! subquery's rows fitting in `work_mem` x `hash_mem_multiplier`. It then
//! keeps two tables, the rows with no NULL and the rows with one, and
//! answers a probe in SQL's three-valued logic:
//! - left side without NULL: TRUE when its row is in the first table; else
//!   NULL when a row of the second matches it on every column both have
//!   (`findPartialMatch`); else FALSE;
//! - left side with a NULL: FALSE for no rows at all, NULL when it is all
//!   NULL, NULL when a row of either table matches it where both have a
//!   value, else FALSE.
//!
//! That is exactly the OR-of-ANDs' answer, which it replaces.
//!
//! Hashing must agree with the `=` the comparison resolves to, so only
//! types whose equality is equality of the value as carried qualify:
//! `text`, `varchar` and `name` (the database's collation is `C`, and no
//! nondeterministic collation or `citext` may exist), the integers, `oid`
//! and `bool`, a domain over one of them, and both sides of a column in the
//! same family. The subquery's column types are checked while planning; the
//! left side's types, which the evaluator knows only as it runs, are checked
//! at the probe. Anything else -- `bpchar`'s blank padding, a collation, a
//! user `=` operator, a value carried in another form -- is answered as the
//! OR of equalities was, in the same order.

use super::*;
use std::collections::HashSet;
use std::sync::Arc;

/// The call standing in for the SubPlan: `__hashed_subplan(lhs..., $rows)`,
/// whose last argument is a parameter holding `{types, rows}`.
pub(crate) const NAME: &str = "__hashed_subplan";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Family {
    Text,
    Int,
    Oid,
    Bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Key {
    Text(String),
    Int(i64),
    Bool(bool),
}

/// The family a type's equality operator hashes in, looking through
/// domains (`getBaseType`).
fn family(ty: &str) -> Option<Family> {
    let mut t = ty.to_string();
    for _ in 0..16 {
        match domains::domain_base(&t) {
            Some(base) if base != t => t = base,
            _ => break,
        }
    }
    match t.as_str() {
        "text" | "varchar" | "name" => Some(Family::Text),
        "int2" | "int4" | "int8" => Some(Family::Int),
        "oid" => Some(Family::Oid),
        "bool" => Some(Family::Bool),
        _ => None,
    }
}

fn key_of(v: &Bson, family: Family) -> Option<Key> {
    match (family, v) {
        (Family::Text, Bson::String(s)) => Some(Key::Text(s.clone())),
        (Family::Int | Family::Oid, Bson::Int32(i)) => Some(Key::Int(i64::from(*i))),
        (Family::Int | Family::Oid, Bson::Int64(i)) => Some(Key::Int(*i)),
        (Family::Bool, Bson::Boolean(b)) => Some(Key::Bool(*b)),
        _ => None,
    }
}

/// Could `=` resolve to anything but the built-in equality, or compare by
/// anything but the value? Then nothing is hashed.
fn equality_is_overridden() -> bool {
    user_ops::defines("=")
        || extension_installed("citext")
        || collation::user_collations()
            .iter()
            .any(|c| !c.deterministic)
}

/// `get_hash_memory_limit`: `work_mem` x `hash_mem_multiplier`, in bytes.
fn hash_memory_limit() -> f64 {
    let work_mem_kb = session_setting("work_mem")
        .and_then(|v| memory_kb(&v))
        .unwrap_or(4096.0);
    let multiplier = session_setting("hash_mem_multiplier")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(2.0);
    work_mem_kb * 1024.0 * multiplier
}

/// A memory GUC's value in kB (its base unit): `4MB`, `64kB`, `4096`.
fn memory_kb(v: &str) -> Option<f64> {
    let v = v.trim();
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let (n, unit) = v.split_at(split);
    let n: f64 = n.parse().ok()?;
    let scale = match unit.trim() {
        "" | "kB" => 1.0,
        "B" => 1.0 / 1024.0,
        "MB" => 1024.0,
        "GB" => 1024.0 * 1024.0,
        "TB" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some(n * scale)
}

/// A column's width as the planner estimates it (`get_typavgwidth`): the
/// fixed length of a fixed-length type, else the values' average length
/// and a varlena header.
fn column_width(ty: &str, rows: &[Vec<Bson>], i: usize) -> f64 {
    let mut t = ty.to_string();
    for _ in 0..16 {
        match domains::domain_base(&t) {
            Some(base) if base != t => t = base,
            _ => break,
        }
    }
    match t.as_str() {
        "name" => 64.0,
        "int2" => 2.0,
        "int4" | "oid" => 4.0,
        "int8" => 8.0,
        "bool" => 1.0,
        _ => {
            let (sum, n) = rows
                .iter()
                .fold((0usize, 0usize), |(s, n), r| match r.get(i) {
                    Some(Bson::String(v)) => (s + v.len(), n + 1),
                    _ => (s, n),
                });
            4.0 + if n == 0 { 0.0 } else { sum as f64 / n as f64 }
        }
    }
}

/// `subplan_is_hashable` for the rows of an uncorrelated `ANY` subquery
/// compared with `=`, given its columns' types: the call that stands in for
/// it, or `None` to keep the OR of equalities.
pub(crate) fn plan(
    lhs: &[pg_query::protobuf::Node],
    rows: &[Vec<Bson>],
    types: &[String],
    params: &mut Vec<Bson>,
) -> Option<pg_query::protobuf::Node> {
    if rows.is_empty() || lhs.is_empty() || types.len() != lhs.len() {
        return None;
    }
    if types.iter().any(|t| family(t).is_none()) || equality_is_overridden() {
        return None;
    }
    // `plan_rows * (MAXALIGN(plan_width) + MAXALIGN(SizeofHeapTupleHeader))`.
    let width: f64 = types
        .iter()
        .enumerate()
        .map(|(i, t)| column_width(t, rows, i))
        .sum();
    let maxalign = |n: f64| ((n.ceil() as u64 + 7) & !7) as f64;
    let size = rows.len() as f64 * (maxalign(width) + 24.0);
    if size > hash_memory_limit() {
        return None;
    }
    let value = Bson::Document(doc! {
        "types": types.iter().map(|t| Bson::String(t.clone())).collect::<Vec<_>>(),
        "rows": rows
            .iter()
            .map(|r| Bson::Array(r.clone()))
            .collect::<Vec<_>>(),
    });
    let mut args = lhs.to_vec();
    args.push(param_node(params, value));
    Some(pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
            funcname: vec![string_node(NAME)],
            args,
            location: -1,
            ..Default::default()
        }))),
    })
}

/// The SubPlan's two hash tables (`hashtable`, `hashnulls`).
pub(crate) struct Table {
    families: Vec<Family>,
    /// Rows with no NULL.
    rows: HashSet<Vec<Key>>,
    /// Rows with a NULL, a column at a time (`None` for the NULL).
    null_rows: Vec<Vec<Option<Key>>>,
    /// Some value is not carried as its family is: the table cannot answer.
    unusable: bool,
}

fn build(types: &[String], rows: &[Bson]) -> Table {
    let families: Vec<Family> = types.iter().filter_map(|t| family(t)).collect();
    let mut table = Table {
        unusable: families.len() != types.len(),
        families,
        rows: HashSet::new(),
        null_rows: Vec::new(),
    };
    for row in rows {
        let Bson::Array(values) = row else {
            table.unusable = true;
            break;
        };
        if values.len() != table.families.len() {
            table.unusable = true;
            break;
        }
        let mut keys = Vec::with_capacity(values.len());
        for (v, f) in values.iter().zip(&table.families) {
            if *v == Bson::Null {
                keys.push(None);
            } else if let Some(k) = key_of(v, *f) {
                keys.push(Some(k));
            } else {
                table.unusable = true;
            }
        }
        if table.unusable {
            break;
        }
        if keys.iter().all(Option::is_some) {
            table.rows.insert(keys.into_iter().flatten().collect());
        } else {
            table.null_rows.push(keys);
        }
    }
    table
}

/// `findPartialMatch`: a row equal to `probe` on every column where both
/// have a value.
fn partial_match<'a>(
    probe: &[Option<Key>],
    mut rows: impl Iterator<Item = &'a [Option<Key>]>,
) -> bool {
    rows.any(|r| {
        r.iter().zip(probe).all(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        })
    })
}

impl Table {
    /// `ExecHashSubPlan`'s answer for one left-hand row.
    fn probe(&self, probe: &[Option<Key>]) -> Option<bool> {
        if probe.iter().all(Option::is_some) {
            let key: Vec<Key> = probe.iter().flatten().cloned().collect();
            if self.rows.contains(&key) {
                return Some(true);
            }
            if partial_match(probe, self.null_rows.iter().map(Vec::as_slice)) {
                return None;
            }
            return Some(false);
        }
        if self.rows.is_empty() && self.null_rows.is_empty() {
            return Some(false);
        }
        if probe.iter().all(Option::is_none) {
            return None;
        }
        if partial_match(probe, self.null_rows.iter().map(Vec::as_slice)) {
            return None;
        }
        if self.rows.iter().any(|r| {
            r.iter().zip(probe).all(|(a, b)| match b {
                Some(b) => a == b,
                None => true,
            })
        }) {
            return None;
        }
        Some(false)
    }
}

/// Evaluate `__hashed_subplan(lhs..., $rows)`.
pub(crate) fn eval(f: &pg_query::protobuf::FuncCall, params: &[Bson]) -> Result<Bson> {
    let malformed = || Error::Internal("a malformed hashed subplan".into());
    let (Some(last), Some(width)) = (f.args.last(), f.args.len().checked_sub(1)) else {
        return Err(malformed());
    };
    let Some(N::ParamRef(p)) = last.node.as_ref() else {
        return Err(malformed());
    };
    let Some(Bson::Document(sub)) = usize::try_from(p.number)
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| params.get(i))
    else {
        return Err(malformed());
    };
    let (Ok(types), Ok(rows)) = (sub.get_array("types"), sub.get_array("rows")) else {
        return Err(malformed());
    };
    let types: Vec<String> = types
        .iter()
        .map(|t| t.as_str().unwrap_or_default().to_string())
        .collect();
    let lhs = &f.args[..width];
    if types.len() != width {
        return Err(malformed());
    }
    let values = lhs
        .iter()
        .map(|a| const_value(a, params))
        .collect::<Result<Vec<_>>>()?;
    // The left side's types, as the comparison would read them.
    let hashable = !equality_is_overridden()
        && lhs.iter().zip(&values).zip(&types).all(|((node, v), rhs)| {
            !matches!(node.node.as_ref(), Some(N::CollateClause(_)))
                && family(&static_type(node, v)).is_some_and(|l| Some(l) == family(rhs))
        });
    if hashable {
        let table = expr_memo::subplan_table(f, || Arc::new(build(&types, rows)))
            .unwrap_or_else(|| Arc::new(build(&types, rows)));
        if !table.unusable {
            let probe: Option<Vec<Option<Key>>> = values
                .iter()
                .zip(&table.families)
                .map(|(v, fam)| {
                    if *v == Bson::Null {
                        Some(None)
                    } else {
                        key_of(v, *fam).map(Some)
                    }
                })
                .collect();
            if let Some(probe) = probe {
                return Ok(table.probe(&probe).map_or(Bson::Null, Bson::Boolean));
            }
        }
    }
    or_of_equalities(lhs, rows, params)
}

/// The OR over the rows of the AND-ed `lhs_i = value` it stands in for,
/// evaluated as the boolean tree was: rows in order, a row's columns in
/// order, stopping at the first TRUE row or FALSE column.
fn or_of_equalities(
    lhs: &[pg_query::protobuf::Node],
    rows: &[Bson],
    params: &[Bson],
) -> Result<Bson> {
    let n = params.len();
    let mut all: Vec<Bson> = params.to_vec();
    all.resize(n + lhs.len(), Bson::Null);
    let comparisons: Vec<pg_query::protobuf::Node> = lhs
        .iter()
        .enumerate()
        .map(|(i, l)| pg_query::protobuf::Node {
            node: Some(N::AExpr(Box::new(AExpr {
                kind: AExprKind::AexprOp as i32,
                name: vec![string_node("=")],
                lexpr: Some(Box::new(l.clone())),
                rexpr: Some(Box::new(pg_query::protobuf::Node {
                    node: Some(N::ParamRef(pg_query::protobuf::ParamRef {
                        number: i32::try_from(n + i + 1).unwrap_or(i32::MAX),
                        location: -1,
                    })),
                })),
                location: -1,
            }))),
        })
        .collect();
    let mut any_null = false;
    for row in rows {
        let Bson::Array(values) = row else {
            return Err(Error::Internal("a malformed hashed subplan".into()));
        };
        let mut row_null = false;
        let mut row_false = false;
        for (i, (cmp, v)) in comparisons.iter().zip(values).enumerate() {
            all[n + i] = v.clone();
            match const_value(cmp, &all)? {
                Bson::Boolean(false) => {
                    row_false = true;
                    break;
                }
                Bson::Boolean(true) => {}
                Bson::Null => row_null = true,
                other => {
                    return Err(Error::DatatypeMismatch(format!(
                        "argument of AND must be type boolean, not type {}",
                        display_type(inferred_type(&other))
                    )))
                }
            }
        }
        if !row_false {
            if !row_null {
                return Ok(Bson::Boolean(true));
            }
            any_null = true;
        }
    }
    Ok(if any_null {
        Bson::Null
    } else {
        Bson::Boolean(false)
    })
}
