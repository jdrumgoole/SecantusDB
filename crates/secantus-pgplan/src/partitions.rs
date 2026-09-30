//! Declarative partitioning: `PARTITION BY`, `PARTITION OF`, and the bounds.
//!
//! The rows of a partition tree live in its ROOT table. A partition is a
//! table in the catalog -- it has columns, shows in `pg_class`, can be named
//! anywhere a table can -- and the executor installs it for the planner as a
//! view over its parent restricted to its bound, checked `CASCADED`. So a read
//! of a partition sees exactly its rows, a write through one is held to its
//! bound, and an `UPDATE` through the parent that changes the key simply moves
//! the row, which is what PostgreSQL 14 does.
//!
//! What this module does is the planning half: it reads the clauses into
//! catalog keys (`partition_by` on a partitioned table, `partition_of` and
//! `partition_bound` on a partition) and lowers `PARTITION OF parent` into
//! `LIKE parent`, so the partition gets its parent's columns. Turning a bound
//! into a condition needs the key columns' types, so that is the executor's,
//! via [`bound_condition`].

use super::*;

/// The SQL of one bound datum, or `MINVALUE` / `MAXVALUE`.
fn datum_sql(n: &pg_query::protobuf::Node) -> Result<String> {
    use pg_query::protobuf::PartitionRangeDatumKind as K;
    match n.node.as_ref() {
        Some(N::PartitionRangeDatum(d)) => match K::try_from(d.kind) {
            Ok(K::PartitionRangeDatumMinvalue) => Ok("MINVALUE".into()),
            Ok(K::PartitionRangeDatumMaxvalue) => Ok("MAXVALUE".into()),
            _ => datum_sql(
                d.value
                    .as_deref()
                    .ok_or_else(|| Error::Parse("empty bound".into()))?,
            ),
        },
        // The raw grammar reads MINVALUE / MAXVALUE as column references.
        Some(N::ColumnRef(c)) => match column_ref_name(c).as_deref() {
            Some("minvalue") => Ok("MINVALUE".into()),
            Some("maxvalue") => Ok("MAXVALUE".into()),
            _ => Err(Error::Sqlstate(
                "42P17",
                "cannot use column reference in partition bound expression".into(),
            )),
        },
        _ => deparse_expr(n),
    }
}

/// A `FOR VALUES` / `DEFAULT` clause as a catalog document.
pub fn bound_document(b: &pg_query::protobuf::PartitionBoundSpec) -> Result<Document> {
    let list = |nodes: &[pg_query::protobuf::Node]| -> Result<Vec<Bson>> {
        nodes
            .iter()
            .map(|n| datum_sql(n).map(Bson::String))
            .collect()
    };
    if b.is_default {
        return Ok(bson::doc! {"kind": "default"});
    }
    match b.strategy.as_str() {
        "r" => Ok(bson::doc! {
            "kind": "range",
            "from": list(&b.lowerdatums)?,
            "to": list(&b.upperdatums)?,
        }),
        "l" => Ok(bson::doc! {"kind": "list", "values": list(&b.listdatums)?}),
        _ => Err(Error::Unsupported("hash partitioning".into())),
    }
}

/// Lower a `CREATE TABLE`'s partitioning clauses: the statement to plan in
/// their place and the catalog keys the new table carries.
pub(crate) fn lower_create(
    c: &pg_query::protobuf::CreateStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<(pg_query::protobuf::CreateStmt, Document)> {
    use pg_query::protobuf::PartitionStrategy as PS;
    let mut out = c.clone();
    let mut extra = Document::new();
    if let Some(spec) = c.partspec.as_ref() {
        let strategy = match PS::try_from(spec.strategy) {
            Ok(PS::Range) => "range",
            Ok(PS::List) => "list",
            _ => return Err(Error::Unsupported("hash partitioning".into())),
        };
        let mut columns = Vec::new();
        for p in &spec.part_params {
            let Some(N::PartitionElem(e)) = p.node.as_ref() else {
                continue;
            };
            if e.expr.is_some() || e.name.is_empty() {
                return Err(Error::Unsupported("partitioning by an expression".into()));
            }
            columns.push(Bson::String(e.name.clone()));
        }
        if strategy == "list" && columns.len() > 1 {
            return Err(Error::Sqlstate(
                "42P17",
                "cannot use \"list\" partition strategy with more than one column".into(),
            ));
        }
        extra.insert(
            "partition_by",
            bson::doc! {"strategy": strategy, "columns": columns},
        );
        out.partspec = None;
    }
    match (c.partbound.as_ref(), c.inh_relations.first()) {
        (Some(bound), Some(parent)) => {
            let Some(N::RangeVar(parent)) = parent.node.as_ref() else {
                return Err(Error::Parse("PARTITION OF without a parent".into()));
            };
            let name = relation_name(parent);
            if lookup(&name).is_none() {
                return Err(Error::UndefinedTable(name));
            }
            extra.insert("partition_of", name);
            extra.insert("partition_bound", bound_document(bound)?);
            // The partition's columns are its parent's.
            const DEFAULTS: u32 = 1 << 3;
            const GENERATED: u32 = 1 << 4;
            let like = pg_query::protobuf::Node {
                node: Some(N::TableLikeClause(pg_query::protobuf::TableLikeClause {
                    relation: Some(parent.clone()),
                    options: DEFAULTS | GENERATED,
                    relation_oid: 0,
                })),
            };
            // Column options (`WITH OPTIONS`) are not modelled; table
            // constraints carry across.
            let mut elts = vec![like];
            for e in &c.table_elts {
                match e.node.as_ref() {
                    Some(N::Constraint(_)) => elts.push(e.clone()),
                    Some(N::ColumnDef(_)) => {
                        return Err(Error::Unsupported("column options on a partition".into()))
                    }
                    _ => {}
                }
            }
            out.table_elts = elts;
            out.inh_relations.clear();
            out.partbound = None;
        }
        (None, Some(_)) => {
            return Err(Error::Unsupported("table inheritance (INHERITS)".into()));
        }
        _ => {}
    }
    Ok((out, extra))
}

/// `quote` a bound datum's canonical text as ruleutils prints it in
/// `pg_get_expr(relpartbound)`: numbers bare, everything else as a literal.
pub fn render_datum(text: &str, pg_type: &str) -> String {
    if text == "MINVALUE" || text == "MAXVALUE" {
        return text.to_string();
    }
    let numeric = matches!(
        pg_type,
        "int2"
            | "int4"
            | "int8"
            | "smallint"
            | "integer"
            | "int"
            | "bigint"
            | "numeric"
            | "decimal"
            | "float4"
            | "float8"
            | "real"
            | "double precision"
            | "oid"
    );
    if numeric && !text.starts_with('-') {
        text.to_string()
    } else if numeric {
        format!("'{text}'")
    } else {
        format!("'{}'", text.replace('\'', "''"))
    }
}

/// The key columns and their types, as bound conditions need them.
pub struct Key<'a> {
    pub columns: Vec<(&'a str, &'a str)>,
}

fn cast(value: &str, ty: &str) -> String {
    // Stored datums are canonical text, so each goes back as a literal of
    // the key column's type.
    format!("'{}'::{ty}", value.replace('\'', "''"))
}

/// The condition a row of this partition satisfies, over the parent's
/// columns (bare names). `siblings` are the other partitions' bounds, which
/// a DEFAULT partition is the complement of. `values` are canonical texts;
/// `None` stands for a NULL list member.
pub fn bound_condition(key: &Key<'_>, bound: &Document, siblings: &[Document]) -> String {
    let q = scalar::quote_identifier;
    let strings = |field: &str| -> Vec<Option<String>> {
        bound
            .get_array(field)
            .map(|a| a.iter().map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };
    match bound.get_str("kind").unwrap_or_default() {
        "list" => {
            let (col, ty) = key.columns[0];
            let values = strings("values");
            let listed: Vec<String> = values.iter().flatten().map(|v| cast(v, ty)).collect();
            let mut parts = Vec::new();
            if !listed.is_empty() {
                parts.push(format!("{} IN ({})", q(col), listed.join(", ")));
            }
            if values.iter().any(Option::is_none) {
                parts.push(format!("{} IS NULL", q(col)));
            }
            if parts.is_empty() {
                "false".into()
            } else {
                format!("({})", parts.join(" OR "))
            }
        }
        "range" => {
            let from = strings("from");
            let to = strings("to");
            let mut parts: Vec<String> = key
                .columns
                .iter()
                .map(|(c, _)| format!("{} IS NOT NULL", q(c)))
                .collect();
            if let Some(lo) = lexicographic(key, &from, ">") {
                parts.push(lo);
            }
            if let Some(hi) = lexicographic(key, &to, "<") {
                parts.push(hi);
            }
            format!("({})", parts.join(" AND "))
        }
        "default" => {
            let others: Vec<String> = siblings
                .iter()
                .filter(|s| s.get_str("kind") != Ok("default"))
                .map(|s| bound_condition(key, s, &[]))
                .collect();
            if others.is_empty() {
                "true".into()
            } else {
                format!("(NOT coalesce(({}), false))", others.join(" OR "))
            }
        }
        _ => "false".into(),
    }
}

/// `(k1, k2, ...) OP (b1, b2, ...)` as PostgreSQL compares a range bound,
/// with `>` meaning "at or above the lower bound" and `<` "below the upper",
/// and MINVALUE / MAXVALUE ending the comparison at their column. `None`
/// when the bound constrains nothing.
fn lexicographic(key: &Key<'_>, bound: &[Option<String>], op: &str) -> Option<String> {
    let q = scalar::quote_identifier;
    let mut alternatives: Vec<String> = Vec::new();
    let mut prefix: Vec<String> = Vec::new();
    for (i, (col, ty)) in key.columns.iter().enumerate() {
        let v = bound.get(i).cloned().flatten().unwrap_or_default();
        match (v.as_str(), op) {
            // Everything is above MINVALUE and below MAXVALUE: the rows equal
            // on the prefix all qualify.
            ("MINVALUE", ">") | ("MAXVALUE", "<") => {
                alternatives.push(conj(&prefix));
                return Some(disj(&alternatives));
            }
            // Nothing is above MAXVALUE or below MINVALUE.
            ("MAXVALUE", ">") | ("MINVALUE", "<") => {
                return if alternatives.is_empty() {
                    Some("false".into())
                } else {
                    Some(disj(&alternatives))
                };
            }
            _ => {}
        }
        let lit = cast(&v, ty);
        let last = i + 1 == key.columns.len();
        let strict = format!("{} {op} {lit}", q(col));
        let mut here = prefix.clone();
        if last && op == ">" {
            here.push(format!("{} >= {lit}", q(col)));
        } else {
            here.push(strict);
        }
        alternatives.push(conj(&here));
        prefix.push(format!("{} = {lit}", q(col)));
    }
    Some(disj(&alternatives))
}

fn conj(parts: &[String]) -> String {
    if parts.is_empty() {
        "true".into()
    } else {
        format!("({})", parts.join(" AND "))
    }
}

fn disj(parts: &[String]) -> String {
    format!("({})", parts.join(" OR "))
}

thread_local! {
    /// `(relation, SQL of its tableoid)`: a constant for a table, a CASE over
    /// the partition bounds for a partitioned one. Published by the executor
    /// beside the views.
    static TABLEOIDS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the `tableoid` expressions for the statements that follow.
pub fn set_tableoids(v: Vec<(String, String)>) {
    TABLEOIDS.with(|t| *t.borrow_mut() = v);
}

fn tableoid_sql(name: &str) -> Option<String> {
    let partitioned = TABLEOIDS.with(|t| {
        t.borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
    });
    // Any other relation is its own oid -- except an inheritance parent,
    // whose rows come from several tables: its union carries the column.
    partitioned.or_else(|| {
        (!is_view(name) && crate::inherit::descendants(name).is_empty())
            .then(|| format!("{}::regclass::oid", crate::scalar::quote_literal(name)))
    })
}

/// Does the statement name the `tableoid` system column anywhere?
pub(crate) fn mentions_tableoid(s: &pg_query::protobuf::SelectStmt) -> bool {
    s.target_list
        .iter()
        .chain(s.where_clause.iter().map(|b| &**b))
        .chain(s.group_clause.iter())
        .chain(s.sort_clause.iter())
        .any(|n| {
            n.node.as_ref().is_some_and(|x| {
                x.nodes().iter().any(|(r, _, _, _)| {
                    matches!(r, pg_query::NodeRef::ColumnRef(c)
                        if matches!(c.fields.last().and_then(|f| f.node.as_ref()),
                            Some(N::String(s)) if s.sval == "tableoid"))
                })
            })
        })
}

/// Replace references to the `tableoid` system column with the expression
/// that computes it, for the relations named directly in this SELECT's
/// FROM. `None` when there is nothing to replace.
pub(crate) fn rewrite_tableoid(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    // (qualifier, relation) for each relation in the FROM list, inside
    // JOINs too.
    fn collect(n: &pg_query::protobuf::Node, out: &mut Vec<(String, String)>) {
        match n.node.as_ref() {
            Some(N::RangeVar(r)) => {
                let alias = r
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| r.relname.clone());
                out.push((alias, r.relname.clone()));
            }
            Some(N::JoinExpr(j)) => {
                if let Some(l) = j.larg.as_deref() {
                    collect(l, out);
                }
                if let Some(r) = j.rarg.as_deref() {
                    collect(r, out);
                }
            }
            _ => {}
        }
    }
    if !mentions_tableoid(s) {
        return Ok(None);
    }
    let mut ranges: Vec<(String, String)> = Vec::new();
    for f in &s.from_clause {
        collect(f, &mut ranges);
    }
    if ranges.iter().all(|(_, r)| tableoid_sql(r).is_none()) {
        return Ok(None);
    }
    let names_tableoid = |c: &pg_query::protobuf::ColumnRef| -> Option<Option<String>> {
        let parts: Vec<String> = c
            .fields
            .iter()
            .filter_map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect();
        match parts.as_slice() {
            [c] if c == "tableoid" => Some(None),
            [q, c] if c == "tableoid" => Some(Some(q.clone())),
            _ => None,
        }
    };
    let mut changed = false;
    let mut replace = |n: &mut pg_query::protobuf::Node| -> Result<()> {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return Ok(());
        };
        let Some(qualifier) = names_tableoid(c) else {
            return Ok(());
        };
        let range = match qualifier {
            Some(q) => ranges.iter().find(|(a, _)| *a == q),
            None if ranges.len() == 1 => ranges.first(),
            None => None,
        };
        let Some((alias, rel)) = range else {
            return Ok(());
        };
        let Some(sql) = tableoid_sql(rel) else {
            return Ok(());
        };
        let mut expr = domains::parse_default_sql(&sql)?;
        // Its column references are the relation's.
        walk_expr(&mut expr, &mut |m| {
            if let Some(N::ColumnRef(cr)) = m.node.as_mut() {
                if cr.fields.len() == 1 {
                    cr.fields.insert(
                        0,
                        pg_query::protobuf::Node {
                            node: Some(N::String(pg_query::protobuf::String {
                                sval: alias.clone(),
                            })),
                        },
                    );
                }
            }
            Ok(())
        })?;
        *n = expr;
        changed = true;
        Ok(())
    };
    let mut out = s.clone();
    for t in &mut out.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                // `tableoid` and `tableoid::regclass` are both output as
                // "tableoid".
                let inner = match v.node.as_ref() {
                    Some(N::TypeCast(tc)) => tc.arg.as_deref(),
                    _ => Some(&*v),
                };
                if rt.name.is_empty() {
                    if let Some(N::ColumnRef(c)) = inner.and_then(|i| i.node.as_ref()) {
                        if names_tableoid(c).is_some() {
                            rt.name = "tableoid".into();
                        }
                    }
                }
                walk_expr(v, &mut replace)?;
            }
        }
    }
    if let Some(w) = out.where_clause.as_deref_mut() {
        walk_expr(w, &mut replace)?;
    }
    for sb in &mut out.sort_clause {
        if let Some(N::SortBy(b)) = sb.node.as_mut() {
            if let Some(n) = b.node.as_deref_mut() {
                walk_expr(n, &mut replace)?;
            }
        }
    }
    for g in &mut out.group_clause {
        walk_expr(g, &mut replace)?;
    }
    Ok(changed.then_some(out))
}
