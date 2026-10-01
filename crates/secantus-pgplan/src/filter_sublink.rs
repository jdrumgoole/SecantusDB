//! An aggregate `FILTER` whose condition holds a subquery.
//!
//! `count(*) FILTER (WHERE EXISTS (SELECT ...))` answered `0A000 SubLink is
//! not supported yet`: the FILTER lowers to an MQL filter, where no subquery
//! can run, while a subquery in an aggregate's ARGUMENT already works. For an
//! aggregate that ignores NULL inputs the two are the same query --
//!
//! ```sql
//! agg(x) FILTER (WHERE p)   ==   agg(CASE WHEN p THEN x END)
//! count(*) FILTER (WHERE p) ==   count(CASE WHEN p THEN 1 END)
//! ```
//!
//! -- because a row the FILTER drops (p false or NULL) becomes a NULL input,
//! which such an aggregate skips. `array_agg` / `json_agg` and friends KEEP
//! NULLs, so they are left alone and still refuse honestly.

use super::*;

/// Aggregates for which a NULL input is the same as no input.
const NULL_SKIPPING: &[&str] = &[
    "count",
    "sum",
    "avg",
    "min",
    "max",
    "string_agg",
    "bool_and",
    "bool_or",
    "every",
    "bit_and",
    "bit_or",
    "bit_xor",
    "stddev",
    "stddev_samp",
    "stddev_pop",
    "variance",
    "var_samp",
    "var_pop",
];

/// Rewrite every such FILTER in `s`'s select list, HAVING and ORDER BY.
pub(crate) fn rewrite(s: &mut pg_query::protobuf::SelectStmt) -> Result<()> {
    for t in &mut s.target_list {
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                walk_expr(v, &mut rewrite_call)?;
            }
        }
    }
    if let Some(h) = s.having_clause.as_deref_mut() {
        walk_expr(h, &mut rewrite_call)?;
    }
    for sb in &mut s.sort_clause {
        if let Some(N::SortBy(b)) = sb.node.as_mut() {
            if let Some(n) = b.node.as_deref_mut() {
                walk_expr(n, &mut rewrite_call)?;
            }
        }
    }
    Ok(())
}

fn rewrite_call(n: &mut pg_query::protobuf::Node) -> Result<()> {
    let Some(N::FuncCall(f)) = n.node.as_mut() else {
        return Ok(());
    };
    let Some(filter) = f.agg_filter.as_deref() else {
        return Ok(());
    };
    if !has_sublink(filter) {
        return Ok(());
    }
    let name = match f.funcname.last().and_then(|p| p.node.as_ref()) {
        Some(N::String(s)) => s.sval.to_ascii_lowercase(),
        _ => return Ok(()),
    };
    if !NULL_SKIPPING.contains(&name.as_str()) || !f.agg_order.is_empty() && name != "string_agg" {
        return Ok(());
    }
    let value = if f.agg_star {
        if name != "count" {
            return Ok(());
        }
        int_const(1)
    } else {
        match f.args.first() {
            Some(a) => a.clone(),
            None => return Ok(()),
        }
    };
    let cond = f.agg_filter.take().map(|b| *b).expect("checked above");
    let case = pg_query::protobuf::Node {
        node: Some(N::CaseExpr(Box::new(pg_query::protobuf::CaseExpr {
            args: vec![pg_query::protobuf::Node {
                node: Some(N::CaseWhen(Box::new(pg_query::protobuf::CaseWhen {
                    expr: Some(Box::new(cond)),
                    result: Some(Box::new(value)),
                    location: -1,
                    ..Default::default()
                }))),
            }],
            location: -1,
            ..Default::default()
        }))),
    };
    if f.agg_star {
        f.agg_star = false;
        f.args = vec![case];
    } else {
        f.args[0] = case;
    }
    Ok(())
}

fn has_sublink(n: &pg_query::protobuf::Node) -> bool {
    let mut n = n.clone();
    let mut found = false;
    let _ = walk_expr(&mut n, &mut |x| {
        if matches!(x.node.as_ref(), Some(N::SubLink(_))) {
            found = true;
        }
        Ok(())
    });
    found
}

fn int_const(v: i32) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::AConst(pg_query::protobuf::AConst {
            val: Some(pg_query::protobuf::a_const::Val::Ival(
                pg_query::protobuf::Integer { ival: v },
            )),
            location: -1,
            ..Default::default()
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rewritten(sql: &str) -> String {
        let mut parsed = pg_query::parse(sql).expect("parses").protobuf;
        let stmt = parsed.stmts[0].stmt.as_mut().expect("a statement");
        let Some(N::SelectStmt(s)) = stmt.node.as_mut() else {
            panic!("a SELECT")
        };
        rewrite(s).expect("rewrites");
        pg_query::deparse(&parsed).expect("deparses")
    }

    #[test]
    fn a_filter_with_a_subquery_becomes_a_case_argument() {
        assert_eq!(
            rewritten("SELECT count(*) FILTER (WHERE EXISTS (SELECT 1 FROM t)) FROM o"),
            "SELECT count(CASE WHEN EXISTS (SELECT 1 FROM t) THEN 1 END) FROM o"
        );
        assert_eq!(
            rewritten("SELECT sum(b) FILTER (WHERE a IN (SELECT x FROM t)) FROM o"),
            "SELECT sum(CASE WHEN a IN (SELECT x FROM t) THEN b END) FROM o"
        );
    }

    #[test]
    fn a_plain_filter_and_a_null_keeping_aggregate_are_left_alone() {
        let plain = "SELECT count(*) FILTER (WHERE a > 1) FROM o";
        assert_eq!(rewritten(plain), plain);
        let keeps_nulls = "SELECT array_agg(a) FILTER (WHERE a IN (SELECT x FROM t)) FROM o";
        assert_eq!(rewritten(keeps_nulls), keeps_nulls);
    }
}
