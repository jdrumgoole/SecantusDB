//! `remove_useless_joins` for an EXPLAINed SELECT (RP-1060): a LEFT JOIN
//! whose inner side is one plain table, read nowhere but in the join's own
//! ON, and unique on the columns that ON equates to the outer side or to a
//! constant, is removed, and the query is planned as its outer side alone
//! (`join_is_removable`). Each outer row then matches at most one inner
//! row, and nothing reads that row, so the join only ever returns its
//! outer rows once each.
//!
//! The planner does not know the unique indexes, so it plans the removed
//! form beside the written one, as `reduce_unique_semijoins` is (RP-411):
//! each [`JoinRemoval`] names the inner table and the columns its ON fixes,
//! and the server takes the removed plan when a unique index of that table
//! -- immediate, without a predicate, on plain columns -- has every key
//! fixed (`relation_has_unique_index_for`).
//!
//! The removed relation stays in PostgreSQL's range table, so EXPLAIN
//! still prefixes the column names it would prefix in a plan of two
//! relations (`useprefix`): [`joins_removed`] tells the planning of the
//! removed form so.
//!
//! Recognised here: an ON equality whose inner operand is a bare column and
//! whose other operand is a column of the outer side, a literal or a cast
//! of either, of the inner column's type or one its B-tree operator family
//! compares across (the integers, the floats, `text` and `varchar`), and a
//! USING column. An equality of any other shape does not fix its column
//! here, which only ever leaves the join in place.

use super::*;
use pg_query::protobuf::{ColumnRef, JoinExpr, JoinType, Node, SelectStmt};

/// A LEFT JOIN the server removes when its inner table is unique on
/// `fixed`.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinRemoval {
    /// The inner table, as the catalog names it.
    pub table: String,
    /// The inner table's columns the join's ON fixes: each equated to a
    /// column of the outer side or to a constant.
    pub fixed: Vec<String>,
    /// The EXPLAIN of the query with the join replaced by its outer side.
    pub removed: Box<Statement>,
}

thread_local! {
    static REMOVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the EXPLAIN being planned is one with a join removed: its range
/// table holds more relations than its FROM shows.
pub(crate) fn joins_removed() -> bool {
    REMOVED.with(std::cell::Cell::get)
}

/// `f`, planned as a query with a join removed.
pub(crate) fn with_joins_removed<R>(f: impl FnOnce() -> R) -> R {
    let was = REMOVED.with(|r| r.replace(true));
    let out = f();
    REMOVED.with(|r| r.set(was));
    out
}

/// A removable LEFT JOIN of `query`: the inner table, the columns its ON
/// fixes, and the query without it.
pub(crate) struct Candidate {
    pub(crate) table: String,
    pub(crate) fixed: Vec<String>,
    pub(crate) query: N,
}

/// Every LEFT JOIN of `query`'s FROM that is removable if its inner table
/// is unique on the columns its ON fixes, in the order the FROM writes
/// them.
pub(crate) fn candidates(query: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Vec<Candidate> {
    let N::SelectStmt(s) = query else {
        return Vec::new();
    };
    // A row mark reads every relation's row identity above the join.
    if s.op != pg_query::protobuf::SetOperation::SetopNone as i32
        || !s.locking_clause.is_empty()
        || s.into_clause.is_some()
        || !s.values_lists.is_empty()
    {
        return Vec::new();
    }
    let ctes: Vec<String> = s
        .with_clause
        .iter()
        .flat_map(|w| w.ctes.iter())
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(c.ctename.clone()),
            _ => None,
        })
        .collect();
    let mut joins = Vec::new();
    for item in &s.from_clause {
        collect_joins(item, &mut joins);
    }
    let mut out = Vec::new();
    for (i, j) in joins.iter().enumerate() {
        let Some((table, fixed)) = removable(s, &joins, i, j, &ctes, lookup) else {
            continue;
        };
        let mut rewritten: SelectStmt = (**s).clone();
        let mut seen = 0;
        for item in &mut rewritten.from_clause {
            if replace_join(item, i, &mut seen) {
                break;
            }
        }
        out.push(Candidate {
            table,
            fixed,
            query: N::SelectStmt(Box::new(rewritten)),
        });
    }
    out
}

/// The FROM item's joins, each before its sides, left before right.
fn collect_joins<'a>(n: &'a Node, out: &mut Vec<&'a JoinExpr>) {
    if let Some(N::JoinExpr(j)) = n.node.as_ref() {
        out.push(j);
        for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
            collect_joins(side, out);
        }
    }
}

/// The `target`-th join (in [`collect_joins`]'s order) replaced by its
/// outer side.
fn replace_join(n: &mut Node, target: usize, seen: &mut usize) -> bool {
    let Some(N::JoinExpr(j)) = n.node.as_mut() else {
        return false;
    };
    if *seen == target {
        if let Some(larg) = j.larg.take() {
            *n = *larg;
        }
        return true;
    }
    *seen += 1;
    for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        if replace_join(side, target, seen) {
            return true;
        }
    }
    false
}

/// The `target`-th join's ON dropped, so that what is left reads only
/// what the query reads outside it.
fn clear_quals(n: &mut Node, target: usize, seen: &mut usize) -> bool {
    let Some(N::JoinExpr(j)) = n.node.as_mut() else {
        return false;
    };
    if *seen == target {
        j.quals = None;
        return true;
    }
    *seen += 1;
    for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
        .into_iter()
        .flatten()
    {
        if clear_quals(side, target, seen) {
            return true;
        }
    }
    false
}

/// A relation of a join's outer side: its name and its table.
struct Rel {
    refname: String,
    def: TableDef,
}

/// The plain relations of a join's outer side, or `None` when it holds
/// anything else (a subquery, a function, a CTE), whose columns are not
/// known here.
fn outer_rels(
    n: &Node,
    ctes: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<Rel>,
) -> Option<()> {
    match n.node.as_ref()? {
        N::RangeVar(rv) if rv.schemaname.is_empty() && !ctes.contains(&rv.relname) => {
            if rv.alias.as_ref().is_some_and(|a| !a.colnames.is_empty()) {
                return None;
            }
            let def = lookup(&rv.relname)?;
            out.push(Rel {
                refname: shown_name(rv),
                def,
            });
            Some(())
        }
        N::JoinExpr(j) => {
            outer_rels(j.larg.as_deref()?, ctes, lookup, out)?;
            outer_rels(j.rarg.as_deref()?, ctes, lookup, out)
        }
        _ => None,
    }
}

fn shown_name(rv: &pg_query::protobuf::RangeVar) -> String {
    rv.alias
        .as_ref()
        .map(|a| a.aliasname.clone())
        .unwrap_or_else(|| rv.relname.clone())
}

fn string_names(list: &[Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

/// Whether `j`, the `index`-th of `joins`, is a LEFT JOIN that is removable
/// if its inner table is unique on the columns returned with it.
fn removable(
    s: &SelectStmt,
    joins: &[&JoinExpr],
    index: usize,
    j: &JoinExpr,
    ctes: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<(String, Vec<String>)> {
    if j.jointype != JoinType::JoinLeft as i32
        || j.is_natural
        || j.alias.is_some()
        || j.join_using_alias.is_some()
    {
        return None;
    }
    let Some(N::RangeVar(rv)) = j.rarg.as_deref()?.node.as_ref() else {
        return None;
    };
    if !rv.schemaname.is_empty()
        || ctes.contains(&rv.relname)
        || rv.alias.as_ref().is_some_and(|a| !a.colnames.is_empty())
    {
        return None;
    }
    // A parent with children reads them too, which its own indexes do
    // not cover (`get_relation_info` gives an inheritance parent none).
    if rv.inh && !crate::inherit::descendants(&rv.relname).is_empty() {
        return None;
    }
    let def = lookup(&rv.relname)?;
    let refname = shown_name(rv);
    let using = string_names(&j.using_clause);
    // Another join's USING or NATURAL can read the inner table's columns
    // by name.
    for (i, other) in joins.iter().enumerate() {
        if i == index {
            continue;
        }
        if other.is_natural
            || string_names(&other.using_clause)
                .iter()
                .any(|u| has_column(&def, u) && !using.contains(u))
        {
            return None;
        }
    }
    // Nothing outside the join's ON may read the inner relation
    // (`attr_needed` within the join).
    let mut probe = s.clone();
    let mut seen = 0;
    for item in &mut probe.from_clause {
        if clear_quals(item, index, &mut seen) {
            break;
        }
    }
    let probe = N::SelectStmt(Box::new(probe));
    if probe.nodes().into_iter().any(|(r, _, _, _)| match r {
        pg_query::NodeRef::ColumnRef(c) => may_read(c, &refname, &def, &using),
        _ => false,
    }) {
        return None;
    }
    let mut outer = Vec::new();
    let outer_complete = outer_rels(j.larg.as_deref()?, ctes, lookup, &mut outer).is_some();
    let mut fixed: Vec<String> = Vec::new();
    for u in &using {
        let Some(inner) = column(&def, u) else {
            continue;
        };
        let mut hits = outer.iter().filter_map(|r| column(&r.def, u));
        if let (Some(o), None, true) = (hits.next(), hits.next(), outer_complete) {
            if compatible(&o.pg_type, &inner.pg_type) && !fixed.contains(u) {
                fixed.push(u.clone());
            }
        }
    }
    let mut conjuncts = Vec::new();
    if let Some(q) = j.quals.as_deref() {
        and_conjuncts(q, &mut conjuncts);
    }
    for c in conjuncts {
        let Some(N::AExpr(e)) = c.node.as_ref() else {
            continue;
        };
        if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || !is_equals(&e.name) {
            continue;
        }
        let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
            continue;
        };
        for (inner, other) in [(l, r), (r, l)] {
            let Some(col) = inner_column(inner, &refname, &def, &outer, outer_complete) else {
                continue;
            };
            let side = Side {
                refname: &refname,
                def: &def,
                outer: &outer,
                outer_complete,
                larg: j.larg.as_deref()?,
                lookup,
            };
            if let Some(t) = other_type(other, &side) {
                if compatible(&t, &col.pg_type) && !fixed.contains(&col.name) {
                    fixed.push(col.name.clone());
                }
            }
            break;
        }
    }
    if fixed.is_empty() {
        return None;
    }
    Some((def.name.clone(), fixed))
}

fn column<'a>(def: &'a TableDef, name: &str) -> Option<&'a Column> {
    def.columns.iter().find(|c| c.name == name)
}

fn has_column(def: &TableDef, name: &str) -> bool {
    column(def, name).is_some()
}

/// A column reference's name parts, `None` for a `*`; `None` overall for
/// a part of any other kind.
fn parts(c: &ColumnRef) -> Option<Vec<Option<&str>>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(Some(s.sval.as_str())),
            Some(N::AStar(_)) => Some(None),
            _ => None,
        })
        .collect()
}

/// Whether a column reference may read the inner relation `refname`: a
/// `*`, its name (a whole-row reference), a name it qualifies, or a bare
/// name that is one of its columns -- other than a USING column of the
/// join, which is the outer side's.
fn may_read(c: &ColumnRef, refname: &str, def: &TableDef, using: &[String]) -> bool {
    let Some(parts) = parts(c) else {
        return true;
    };
    match parts.as_slice() {
        [None] => true,
        [Some(n)] => *n == refname || (has_column(def, n) && !using.iter().any(|u| u == n)),
        [.., q, _] => *q == Some(refname),
        [] => true,
    }
}

fn and_conjuncts<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                and_conjuncts(a, out);
            }
        }
        _ => out.push(n),
    }
}

fn is_equals(names: &[Node]) -> bool {
    matches!(
        names,
        [n] if matches!(n.node.as_ref(), Some(N::String(s)) if s.sval == "=")
    )
}

/// The inner relation's column `n` is, when it is a bare reference to one.
fn inner_column<'a>(
    n: &Node,
    refname: &str,
    def: &'a TableDef,
    outer: &[Rel],
    outer_complete: bool,
) -> Option<&'a Column> {
    let Some(N::ColumnRef(c)) = n.node.as_ref() else {
        return None;
    };
    match parts(c)?.as_slice() {
        [Some(q), Some(col)] if *q == refname => column(def, col),
        [Some(col)] if outer_complete && !outer.iter().any(|r| has_column(&r.def, col)) => {
            column(def, col)
        }
        _ => None,
    }
}

/// What an equality's other operand is typed against: the inner relation
/// it must not read, and the join's outer side.
struct Side<'a> {
    refname: &'a str,
    def: &'a TableDef,
    outer: &'a [Rel],
    outer_complete: bool,
    larg: &'a Node,
    lookup: &'a dyn Fn(&str) -> Option<TableDef>,
}

/// The type of an equality's operand over the outer side or constants: a
/// column of the outer side, a literal (`unknown` for a quoted one, which
/// takes the other operand's type), or an expression over those, typed in
/// the outer side's scope. `None` for an operand that may read the inner
/// relation, holds a subquery or a volatile function (not mergejoinable,
/// `check_mergejoinable`), or is not typed here.
fn other_type(n: &Node, side: &Side<'_>) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    let (refname, def, outer, outer_complete) =
        (side.refname, side.def, side.outer, side.outer_complete);
    match n.node.as_ref()? {
        N::AConst(c) if !c.isnull => match c.val.as_ref()? {
            Val::Ival(_) => Some("int4".into()),
            Val::Fval(f) if f.fval.bytes().all(|b| b.is_ascii_digit()) => Some(
                if f.fval.parse::<i64>().is_ok() {
                    "int8"
                } else {
                    "numeric"
                }
                .into(),
            ),
            Val::Fval(_) => Some("numeric".into()),
            Val::Sval(_) => Some("unknown".into()),
            _ => None,
        },
        N::TypeCast(tc)
            if matches!(
                tc.arg.as_deref().and_then(|a| a.node.as_ref()),
                Some(N::ColumnRef(_) | N::AConst(_))
            ) =>
        {
            other_type(tc.arg.as_deref()?, side)?;
            let t = type_name_of(tc.type_name.as_ref()?);
            (!t.ends_with("[]")).then_some(t)
        }
        N::ColumnRef(c) => {
            if may_read(c, refname, def, &[]) {
                return None;
            }
            match parts(c)?.as_slice() {
                [Some(q), Some(col)] => {
                    let rel = outer.iter().find(|r| r.refname == *q)?;
                    Some(column(&rel.def, col)?.pg_type.clone())
                }
                [Some(col)] if outer_complete => {
                    let mut hits = outer.iter().filter_map(|r| column(&r.def, col));
                    match (hits.next(), hits.next()) {
                        (Some(c), None) => Some(c.pg_type.clone()),
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        N::AConst(_) => None,
        e => {
            let readable = e.nodes().into_iter().all(|(r, _, _, _)| match r {
                pg_query::NodeRef::ColumnRef(c) => !may_read(c, refname, def, &[]),
                pg_query::NodeRef::SubLink(_) => false,
                pg_query::NodeRef::FuncCall(f) => crate::func_name(f).is_some_and(|name| {
                    !crate::correlated::volatile_builtin(&name, f.args.len())
                        && !crate::correlated::user_function_named(&name)
                }),
                _ => true,
            });
            if !readable {
                return None;
            }
            let scope = SelectStmt {
                from_clause: vec![side.larg.clone()],
                ..Default::default()
            };
            crate::optype::type_in_select(&scope, n, side.lookup)
        }
    }
}

/// Whether an equality of a `other` operand with an inner column of type
/// `inner` is one of the inner column's B-tree operator family: the same
/// type, an untyped literal, or a type of the same cross-type family.
fn compatible(other: &str, inner: &str) -> bool {
    let family = |t: &str| match t {
        "int2" | "int4" | "int8" => 1,
        "float4" | "float8" => 2,
        "text" | "varchar" => 3,
        _ => 0,
    };
    other == "unknown" || other == inner || (family(other) != 0 && family(other) == family(inner))
}

#[cfg(test)]
mod rp1060_join_removal {
    use super::*;

    /// Each candidate's inner table and fixed columns, and the query
    /// without it.
    fn removals(sql: &str) -> Vec<(String, Vec<String>, String)> {
        let lookup = |name: &str| -> Option<TableDef> {
            let cols: &[(&str, &str)] = match name {
                "e" => &[("a", "int4"), ("b", "text")],
                "k" => &[("a", "int4"), ("b", "text")],
                "h" => &[("a", "int8"), ("b", "text")],
                "n" => &[("a", "numeric"), ("b", "text")],
                _ => return None,
            };
            Some(TableDef::new(
                name,
                cols.iter()
                    .map(|(c, t)| secantus_pgcatalog::Column::new(c, t, false))
                    .collect(),
            ))
        };
        let tree = pg_query::parse(sql).unwrap();
        let q = tree.protobuf.stmts[0]
            .stmt
            .as_ref()
            .unwrap()
            .node
            .clone()
            .unwrap();
        candidates(&q, &lookup)
            .into_iter()
            .map(|c| {
                let text = Node {
                    node: Some(c.query),
                }
                .deparse()
                .unwrap();
                (c.table, c.fixed, text)
            })
            .collect()
    }

    fn one(table: &str, fixed: &[&str], sql: &str) -> Vec<(String, Vec<String>, String)> {
        vec![(
            table.to_string(),
            fixed.iter().map(|f| f.to_string()).collect(),
            sql.to_string(),
        )]
    }

    /// A LEFT JOIN read only in its own ON is a candidate, fixed on the
    /// columns that ON equates to the outer side or to a constant
    /// (`join_is_removable`, RP-1060).
    #[test]
    fn a_left_join_read_only_in_its_on_is_a_candidate() {
        let removed = "SELECT e.b FROM e";
        assert_eq!(
            removals("select e.b from e left join k on e.a = k.a"),
            one("k", &["a"], removed)
        );
        assert_eq!(
            removals("select e.b from e left join k on k.a = 3 and k.b = 'x'"),
            one("k", &["a", "b"], removed)
        );
        // Across integer widths, and over an outer expression.
        assert_eq!(
            removals("select e.b from e left join h on e.a = h.a"),
            one("h", &["a"], removed)
        );
        assert_eq!(
            removals("select e.b from e left join k on e.a + 1 = k.a"),
            one("k", &["a"], removed)
        );
        assert_eq!(
            removals("select a from e left join k using (a)"),
            one("k", &["a"], "SELECT a FROM e")
        );
        assert_eq!(
            removals("select e.b from e left join k on e.a = k.a left join h on h.a = e.a").len(),
            2
        );
    }

    /// A join something above it reads, or whose equality is no use to a
    /// unique index, is not.
    #[test]
    fn a_left_join_read_above_or_unprovable_is_not() {
        for sql in [
            "select * from e left join k on e.a = k.a",
            "select e.b, k.b from e left join k on e.a = k.a",
            "select e.b from e left join k on e.a = k.a where k.b is null",
            "select e.b from e left join k on e.a = k.a order by k.b",
            "select count(k) from e left join k on e.a = k.a",
            "select e.b from e join k on e.a = k.a",
            "select e.b from e left join k on e.a < k.a",
            "select e.b from e left join k on k.a = e.a + k.a",
            "select e.b from e left join k on (random() * 10)::int = k.a",
            // A numeric equality casts the inner column.
            "select e.b from e left join n on e.a = n.a",
            "select e.b from e left join k on k.a = 3.5",
            "select e.b from e left join k on e.a = k.a for update",
            "with k as (select 1 as a) select e.b from e left join k on k.a = e.a",
        ] {
            assert_eq!(removals(sql), Vec::new(), "{sql}");
        }
    }
}
