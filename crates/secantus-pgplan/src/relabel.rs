//! `eval_const_expressions`' collapse of a RelabelType chain in a planned
//! comparison (RP-912): a cast between binary-compatible string types
//! (`text` and `varchar`, `pg_cast.castmethod = 'b'`) is a RelabelType, and
//! where the comparison then coerces the operand back to `text` for its
//! operator the two relabels collapse into none. `b::varchar <> 'x'` over
//! a text `b` plans as `b <> 'x'::text`, so the column keeps its
//! statistics (`examine_variable`) and EXPLAIN prints it bare.
//!
//! Only a comparison whose operator is `text`'s either way is touched: the
//! other operand `text`, `varchar` or an untyped literal. Against a
//! `char(n)` or a `name` the cast picks the operator (`bpchareq`,
//! `texteqname`), so it stays.

use super::*;
use pg_query::protobuf::Node;

fn string_relabel(t: &str) -> bool {
    matches!(t, "text" | "varchar")
}

/// The relations a FROM item names: (the name it goes by, the table).
fn relations(
    n: &Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<(String, TableDef)>,
) {
    match n.node.as_ref() {
        Some(N::RangeVar(rv)) => {
            if let Some(def) = lookup(&rv.relname) {
                let name = rv
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_else(|| rv.relname.clone());
                out.push((name, def));
            }
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                relations(side, lookup, out);
            }
        }
        _ => {}
    }
}

/// The declared type of a column reference, when exactly one relation has
/// it (or the one it is qualified by).
fn column_type(n: &Node, rels: &[(String, TableDef)]) -> Option<String> {
    let Some(N::ColumnRef(c)) = n.node.as_ref() else {
        return None;
    };
    let parts: Vec<String> = c
        .fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect::<Option<_>>()?;
    let mut found = rels
        .iter()
        .filter_map(|(name, def)| match parts.as_slice() {
            [col] => def.column(col),
            [rel, col] if rel == name => def.column(col),
            _ => None,
        });
    let col = found.next()?;
    if found.next().is_some() {
        return None;
    }
    Some(ruleutils::base_type(&col.pg_type))
}

/// The column a binary-coercible string cast relabels, with its type.
fn relabelled_column(n: &Node, rels: &[(String, TableDef)]) -> Option<Node> {
    let Some(N::TypeCast(tc)) = n.node.as_ref() else {
        return None;
    };
    let tn = tc.type_name.as_ref()?;
    if !tn.typmods.is_empty() || !tn.array_bounds.is_empty() || tn.setof || tn.pct_type {
        return None;
    }
    if !string_relabel(&ruleutils::base_type(&type_name_of(tn))) {
        return None;
    }
    let arg = tc.arg.as_deref()?;
    // A cast of a cast collapses too (`b::varchar::text`).
    let inner = relabelled_column(arg, rels).unwrap_or_else(|| arg.clone());
    string_relabel(&column_type(&inner, rels)?).then_some(inner)
}

/// Whether the other operand's type keeps the comparison on `text`.
fn text_operand(n: &Node, rels: &[(String, TableDef)]) -> bool {
    match n.node.as_ref() {
        Some(N::AConst(c)) => matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))),
        Some(N::ColumnRef(_)) => column_type(n, rels).is_some_and(|t| string_relabel(&t)),
        Some(N::TypeCast(_)) => relabelled_column(n, rels).is_some(),
        _ => false,
    }
}

fn strip(n: &mut Node, rels: &[(String, TableDef)]) -> Result<()> {
    walk_expr(n, &mut |x| {
        let Some(N::AExpr(e)) = x.node.as_mut() else {
            return Ok(());
        };
        if AExprKind::try_from(e.kind) != Ok(AExprKind::AexprOp)
            || !matches!(
                operator_name(e).ok(),
                Some("=" | "<>" | "<" | "<=" | ">" | ">=")
            )
        {
            return Ok(());
        }
        let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
            return Ok(());
        };
        let (lc, rc) = (relabelled_column(l, rels), relabelled_column(r, rels));
        if lc.is_none() && rc.is_none() {
            return Ok(());
        }
        if !text_operand(l, rels) || !text_operand(r, rels) {
            return Ok(());
        }
        if let Some(c) = lc {
            e.lexpr = Some(Box::new(c));
        }
        if let Some(c) = rc {
            e.rexpr = Some(Box::new(c));
        }
        Ok(())
    })
}

/// Collapse the relabels of an EXPLAINed SELECT's WHERE and JOIN ONs.
pub(crate) fn strip_relabels(
    query: &mut N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Result<()> {
    let N::SelectStmt(s) = query else {
        return Ok(());
    };
    let mut rels = Vec::new();
    for f in &s.from_clause {
        relations(f, lookup, &mut rels);
    }
    if rels.is_empty() {
        return Ok(());
    }
    fn ons(n: &mut Node, rels: &[(String, TableDef)]) -> Result<()> {
        if let Some(N::JoinExpr(j)) = n.node.as_mut() {
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                ons(side, rels)?;
            }
            if let Some(q) = j.quals.as_deref_mut() {
                strip(q, rels)?;
            }
        }
        Ok(())
    }
    for f in &mut s.from_clause {
        ons(f, &rels)?;
    }
    if let Some(w) = s.where_clause.as_deref_mut() {
        strip(w, &rels)?;
    }
    Ok(())
}
