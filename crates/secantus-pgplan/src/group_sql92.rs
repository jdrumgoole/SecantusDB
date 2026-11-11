//! A GROUP BY constant that is no integer (RP-854).
//!
//! `findTargetlistEntrySQL92` reads a GROUP BY item that is a bare
//! constant as a select-list position, and refuses any other constant --
//! a numeric (`1.5`, `1e3`, an integer too wide for int4), a string, a bit
//! string, a boolean, NULL -- as 42601 `non-integer constant in GROUP BY`
//! at the constant. Only a constant as written counts: `'x'::text` and
//! `'x' || 'y'` are expressions, and group by their value. Parentheses
//! make no node, so `(1.5)` is the constant, located at itself; a negated
//! constant is folded into one (`doNegate`), located at its `-`.
//!
//! `transformGroupClause` flattens the list first (`flatten_grouping_sets`:
//! an implicit row `(a, b)` is its elements, and a grouping set is
//! analysed element by element), then analyses each item in order, so an
//! item before the constant -- `group by 9, 1.5`, `rollup(9, 1.5)` --
//! fails first with its own error.
//!
//! It runs after the FROM list, the select list, WHERE, HAVING and ORDER
//! BY are analysed (`transformSelectStmt`), so an error of theirs comes
//! first, and before what follows the grouping clause: the ungrouped-column
//! and `GROUPING()` checks (`parseCheckAggregates`), the locking clause
//! (`CheckSelectLocking`), and anything the planner raises. The statement
//! is therefore planned first with its GROUP BY cut short before the
//! constant, and an error from that plan that PostgreSQL raises in the
//! clauses analysed earlier is the answer; anything else, or no error, is
//! the constant's 42601.

use super::*;

use pg_query::protobuf::{a_const, CoercionForm, Node, SelectStmt};

/// The location of the first non-integer constant among `items` in
/// `transformGroupClause`'s order, with `items` cut to what comes before
/// it: an item containing it keeps only its own elements before it.
fn cut_at_constant(items: &[Node]) -> Option<(i32, Vec<Node>)> {
    for (i, item) in items.iter().enumerate() {
        let inner = match item.node.as_ref() {
            Some(N::AConst(c)) if !matches!(c.val, Some(a_const::Val::Ival(_))) => {
                return Some((c.location, items[..i].to_vec()));
            }
            Some(N::RowExpr(r)) if r.row_format == CoercionForm::CoerceImplicitCast as i32 => {
                cut_at_constant(&r.args).map(|(at, kept)| {
                    let mut row = r.clone();
                    row.args = kept;
                    (at, N::RowExpr(row))
                })
            }
            Some(N::GroupingSet(g)) => cut_at_constant(&g.content).map(|(at, kept)| {
                let mut set = g.clone();
                set.content = kept;
                (at, N::GroupingSet(set))
            }),
            _ => None,
        };
        if let Some((at, cut)) = inner {
            let mut kept = items[..i].to_vec();
            kept.push(Node { node: Some(cut) });
            return Some((at, kept));
        }
    }
    None
}

/// Does PostgreSQL raise `e` while analysing the clauses before GROUP BY,
/// so that it comes before the constant's error? Not this server's own
/// gaps, not the checks that follow the grouping clause, and not what only
/// a plan or a run raises.
fn raised_before_group_by(e: &Error) -> bool {
    let m = e.to_string();
    match e {
        Error::Unsupported(_) | Error::DivisionByZero => false,
        // `CheckSelectLocking`, from `transformLockingClause`.
        Error::FeatureNotSupported(_) => !m.contains(" is not allowed with "),
        // `check_ungrouped_columns` and the GROUPING() check, in
        // `parseCheckAggregates`; an aggregate misplaced in WHERE is found
        // while WHERE is analysed.
        _ if e.sqlstate() == "42803" => {
            !(m.contains(" must appear in the GROUP BY clause")
                || m.contains("subquery uses ungrouped column")
                || m.starts_with("arguments to GROUPING must be grouping expressions"))
        }
        _ => true,
    }
}

/// PostgreSQL's 42601 for a GROUP BY constant that is no integer, unless
/// the statement fails earlier (module docs). `plan` plans a statement.
pub(crate) fn check(s: &SelectStmt, plan: &dyn Fn(&SelectStmt) -> Result<Statement>) -> Result<()> {
    let Some((at, kept)) = cut_at_constant(&s.group_clause) else {
        return Ok(());
    };
    let mut cut = s.clone();
    cut.group_clause = kept;
    if let Err(e) = plan(&cut) {
        if raised_before_group_by(&e) {
            return Err(e);
        }
    }
    set_error_location(at);
    Err(Error::Sqlstate(
        "42601",
        "non-integer constant in GROUP BY".into(),
    ))
}
