//! A JOIN of more than two relations, rewritten into the two-sided joins the
//! planner knows.
//!
//! `plan_join_select` joins exactly two sides, each a table or a subquery.
//! psql's `\dt` writes `pg_class c LEFT JOIN pg_namespace n ON ... LEFT JOIN
//! pg_am am ON ...`, and its `\d` a four-relation comma FROM. Both are the
//! same shape once the earlier relations are folded into one subquery side:
//!
//! ```sql
//! SELECT c.relname, n.nspname, am.amname
//! FROM (SELECT c.relname AS "c.relname", ..., n.nspname AS "n.nspname", ...
//!       FROM pg_class c LEFT JOIN pg_namespace n ON n.oid = c.relnamespace)
//!      "__join_c_n"
//!      LEFT JOIN pg_am am ON am.oid = "__join_c_n"."c.relam"
//! ```
//!
//! Every column of every folded relation is projected under its qualified
//! name, and each reference to a folded alias in the rest of the statement is
//! redirected to the subquery's column of that name. The output name of an
//! unaliased `c.relname` stays `relname`, which is what the client sees. The
//! rewrite is applied once per pass and the planner recurses, so a chain of
//! any length folds down to two sides.

use pg_query::protobuf::{self as pb, node::Node as N, Node};

use crate::{Error, Result, TableDef};

/// The folded relations of one nested side: (alias, its columns).
type Leaves = Vec<(String, TableDef)>;

fn is_join(n: Option<&Node>) -> bool {
    matches!(n.and_then(|x| x.node.as_ref()), Some(N::JoinExpr(_)))
}

/// Whether `s`'s FROM has more than two relations in one join tree.
pub(crate) fn has_nested_join(s: &pb::SelectStmt) -> bool {
    match s.from_clause.as_slice() {
        [one] => match one.node.as_ref() {
            Some(N::JoinExpr(j)) => is_join(j.larg.as_deref()) || is_join(j.rarg.as_deref()),
            _ => false,
        },
        [a, b] => is_join(Some(a)) || is_join(Some(b)),
        items => items.len() > 2,
    }
}

/// Every table or subquery under `node`, with its columns.
fn leaves(
    node: &Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[bson::Bson],
    out: &mut Leaves,
) -> Result<()> {
    match node.node.as_ref() {
        Some(N::RangeVar(r)) => {
            let table = crate::range_var_name(r);
            let alias = r
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_else(|| r.relname.clone());
            let def = lookup(&table).ok_or_else(|| Error::UndefinedTable(table.clone()))?;
            out.push((alias, def));
        }
        Some(N::RangeSubselect(rs)) => {
            let alias = rs
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .ok_or_else(|| Error::Unsupported("a JOIN subquery without an alias".into()))?;
            let derived = crate::plan_derived(rs, lookup, params)?;
            out.push((alias, derived.def));
        }
        Some(N::JoinExpr(j)) => {
            if let Some(l) = j.larg.as_deref() {
                leaves(l, lookup, params, out)?;
            }
            if let Some(r) = j.rarg.as_deref() {
                leaves(r, lookup, params, out)?;
            }
        }
        _ => return Err(Error::Unsupported("this JOIN side".into())),
    }
    Ok(())
}

fn string_node(s: &str) -> Node {
    Node {
        node: Some(N::String(pb::String {
            sval: s.to_string(),
        })),
    }
}

fn column_ref(alias: &str, col: &str) -> Node {
    Node {
        node: Some(N::ColumnRef(pb::ColumnRef {
            fields: vec![string_node(alias), string_node(col)],
            location: -1,
        })),
    }
}

fn res_target(name: &str, val: Node) -> Node {
    Node {
        node: Some(N::ResTarget(Box::new(pb::ResTarget {
            name: name.to_string(),
            indirection: Vec::new(),
            val: Some(Box::new(val)),
            location: -1,
        }))),
    }
}

/// The parts of a ColumnRef: (alias, column) for `a.c`, (None, column) for
/// a bare `c`; `None` for a star or anything longer.
fn ref_parts(c: &pb::ColumnRef) -> Option<(Option<String>, String)> {
    let part = |n: &Node| match n.node.as_ref() {
        Some(N::String(s)) => Some(s.sval.clone()),
        _ => None,
    };
    match c.fields.as_slice() {
        [col] => Some((None, part(col)?)),
        [alias, col] => Some((Some(part(alias)?), part(col)?)),
        _ => None,
    }
}

/// `s` with its first nested join (or the leading relations of a comma FROM
/// of three or more) folded into a subquery side, or `None` when its FROM is
/// already two-sided.
pub(crate) fn flatten_from(
    s: &pb::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[bson::Bson],
) -> Result<Option<pb::SelectStmt>> {
    if !has_nested_join(s) {
        return Ok(None);
    }
    let mut out = s.clone();
    let single_join = out.from_clause.len() == 1;
    // Take the nested subtree out of the statement before the reference
    // rewrite walks it: its own references must stay as written.
    let nested_from: Vec<Node> = match out.from_clause.len() {
        1 => {
            let Some(N::JoinExpr(j)) = out.from_clause[0].node.as_mut() else {
                return Ok(None);
            };
            let slot = if is_join(j.larg.as_deref()) {
                &mut j.larg
            } else {
                &mut j.rarg
            };
            vec![*slot.take().expect("checked")]
        }
        2 => {
            let i = usize::from(!is_join(Some(&out.from_clause[0])));
            vec![std::mem::take(&mut out.from_clause[i])]
        }
        n => out.from_clause.drain(..n - 1).collect(),
    };
    let mut folded = Leaves::new();
    for n in &nested_from {
        leaves(n, lookup, params, &mut folded)?;
    }
    let synthetic = format!(
        "__join_{}",
        folded
            .iter()
            .map(|(a, _)| a.as_str())
            .collect::<Vec<_>>()
            .join("_")
    );
    let projected = |alias: &str, col: &str| column_ref(&synthetic, &format!("{alias}.{col}"));
    let leaf = |alias: &str| folded.iter().find(|(a, _)| a == alias);
    // A bare column name resolves to the one folded relation that has it.
    let bare_owner = |col: &str| -> Option<&str> {
        let mut owners = folded
            .iter()
            .filter(|(_, def)| def.column(col).is_some())
            .map(|(a, _)| a.as_str());
        let first = owners.next()?;
        owners.next().is_none().then_some(first)
    };

    // `c.*` over a folded relation expands to its columns, each under its
    // bare name.
    let mut targets = Vec::with_capacity(out.target_list.len());
    for t in std::mem::take(&mut out.target_list) {
        let star_of = match t.node.as_ref() {
            Some(N::ResTarget(rt)) => match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                Some(N::ColumnRef(c)) => match c.fields.as_slice() {
                    [alias, star] if matches!(star.node.as_ref(), Some(N::AStar(_))) => match alias
                        .node
                        .as_ref()
                    {
                        Some(N::String(a)) => leaf(&a.sval).map(|(a, d)| (a.clone(), d.clone())),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        };
        match star_of {
            Some((alias, def)) => {
                for c in &def.columns {
                    targets.push(res_target(&c.name, projected(&alias, &c.name)));
                }
            }
            None => targets.push(t),
        }
    }
    out.target_list = targets;

    let mut rewrite = |node: &mut Node| -> bool {
        match node.node.as_mut() {
            // An unaliased `c.relname` keeps `relname` as its output name.
            Some(N::ResTarget(rt)) if rt.name.is_empty() => {
                if let Some(N::ColumnRef(c)) = rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                    if let Some((alias, col)) = ref_parts(c) {
                        let folded_ref = match alias {
                            Some(a) => leaf(&a).is_some(),
                            None => bare_owner(&col).is_some(),
                        };
                        if folded_ref {
                            rt.name = col;
                        }
                    }
                }
            }
            Some(N::ColumnRef(c)) => {
                if let Some((alias, col)) = ref_parts(c) {
                    let owner = match alias {
                        Some(a) => leaf(&a).map(|(a, _)| a.clone()),
                        None => bare_owner(&col).map(str::to_string),
                    };
                    if let Some(owner) = owner {
                        c.fields = vec![
                            string_node(&synthetic),
                            string_node(&format!("{owner}.{col}")),
                        ];
                    }
                }
            }
            _ => {}
        }
        false
    };
    crate::walk::walk_select(&mut out, &mut rewrite);

    // The folded side: every column of every folded relation, qualified.
    let mut inner_targets = Vec::new();
    for (alias, def) in &folded {
        for c in &def.columns {
            inner_targets.push(res_target(
                &format!("{alias}.{}", c.name),
                column_ref(alias, &c.name),
            ));
        }
    }
    let inner = pb::SelectStmt {
        target_list: inner_targets,
        from_clause: nested_from,
        limit_option: pb::LimitOption::Default as i32,
        op: pb::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    let side = Node {
        node: Some(N::RangeSubselect(Box::new(pb::RangeSubselect {
            lateral: false,
            subquery: Some(Box::new(Node {
                node: Some(N::SelectStmt(Box::new(inner))),
            })),
            alias: Some(pb::Alias {
                aliasname: synthetic.clone(),
                colnames: Vec::new(),
            }),
        }))),
    };
    if single_join {
        let Some(N::JoinExpr(j)) = out.from_clause[0].node.as_mut() else {
            return Ok(None);
        };
        if j.larg.is_none() {
            j.larg = Some(Box::new(side));
        } else {
            j.rarg = Some(Box::new(side));
        }
    } else if let Some(i) = out.from_clause.iter().position(|n| n.node.is_none()) {
        // The two-item form: the folded join goes back where it was taken from.
        out.from_clause[i] = side;
    } else {
        // The comma form: the folded relations lead, in written order.
        out.from_clause.insert(0, side);
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Column;

    fn lookup(name: &str) -> Option<TableDef> {
        let cols = |names: &[&str]| names.iter().map(|n| Column::new(n, "int4", true)).collect();
        match name {
            "a" => Some(TableDef::new("a", cols(&["id", "x"]))),
            "b" => Some(TableDef::new("b", cols(&["id", "a_id"]))),
            "c" => Some(TableDef::new("c", cols(&["id", "b_id"]))),
            _ => None,
        }
    }

    fn flattened(sql: &str) -> String {
        let tree = pg_query::parse(sql).unwrap().protobuf;
        let Some(N::SelectStmt(s)) = tree.stmts[0].stmt.as_ref().and_then(|n| n.node.as_ref())
        else {
            panic!("not a select");
        };
        let out = flatten_from(s, &lookup, &[]).unwrap().expect("nested");
        let tree = pb::ParseResult {
            version: tree.version,
            stmts: vec![pb::RawStmt {
                stmt: Some(Box::new(Node {
                    node: Some(N::SelectStmt(Box::new(out))),
                })),
                stmt_location: 0,
                stmt_len: 0,
            }],
        };
        pg_query::ParseResult::new(tree, String::new())
            .deparse()
            .unwrap()
    }

    #[test]
    fn three_way_join_chain_folds_the_first_two() {
        assert_eq!(
            flattened(
                "select a.x, b.id as bid, c.id from a join b on b.a_id = a.id \
                 left join c on c.b_id = b.id where a.x > 1 order by a.id"
            ),
            "SELECT __join_a_b.\"a.x\" AS x, __join_a_b.\"b.id\" AS bid, c.id \
             FROM (SELECT a.id AS \"a.id\", a.x AS \"a.x\", b.id AS \"b.id\", b.a_id AS \"b.a_id\" \
             FROM a JOIN b ON b.a_id = a.id) __join_a_b \
             LEFT JOIN c ON c.b_id = __join_a_b.\"b.id\" \
             WHERE __join_a_b.\"a.x\" > 1 ORDER BY __join_a_b.\"a.id\""
        );
    }

    #[test]
    fn comma_from_of_three_folds_the_leading_two() {
        assert_eq!(
            flattened("select x, c.id from a, b, c where a.id = b.a_id and b.id = c.b_id"),
            "SELECT __join_a_b.\"a.x\" AS x, c.id \
             FROM (SELECT a.id AS \"a.id\", a.x AS \"a.x\", b.id AS \"b.id\", b.a_id AS \"b.a_id\" \
             FROM a, b) __join_a_b, c \
             WHERE __join_a_b.\"a.id\" = __join_a_b.\"b.a_id\" AND __join_a_b.\"b.id\" = c.b_id"
        );
    }

    #[test]
    fn star_of_a_folded_relation_expands() {
        assert_eq!(
            flattened("select b.* from a join b on b.a_id = a.id join c on c.b_id = b.id"),
            "SELECT __join_a_b.\"b.id\" AS id, __join_a_b.\"b.a_id\" AS a_id \
             FROM (SELECT a.id AS \"a.id\", a.x AS \"a.x\", b.id AS \"b.id\", b.a_id AS \"b.a_id\" \
             FROM a JOIN b ON b.a_id = a.id) __join_a_b JOIN c ON c.b_id = __join_a_b.\"b.id\""
        );
    }

    #[test]
    fn two_sided_joins_are_left_alone() {
        let tree = pg_query::parse("select 1 from a join b on b.a_id = a.id")
            .unwrap()
            .protobuf;
        let Some(N::SelectStmt(s)) = tree.stmts[0].stmt.as_ref().and_then(|n| n.node.as_ref())
        else {
            panic!("not a select");
        };
        assert!(flatten_from(s, &lookup, &[]).unwrap().is_none());
    }
}
