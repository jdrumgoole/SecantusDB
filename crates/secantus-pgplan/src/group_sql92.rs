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
//! the constant's 42601. DISTINCT and DISTINCT ON are analysed after GROUP
//! BY, so the cut drops them too.
//!
//! A DISTINCT ON constant goes through the same `findTargetlistEntrySQL92`
//! (RP-872): 42601 `non-integer constant in DISTINCT ON` at the constant.
//! `transformDistinctOnClause` analyses its items in order after GROUP BY,
//! and only then checks them against the ORDER BY, so the statement is
//! planned with the DISTINCT ON cut before the constant, and with nothing
//! analysed after it (LIMIT, OFFSET, the locking clause); the ORDER BY
//! match and what `parseCheckAggregates` raises are not the answer. Its
//! items are not flattened: a row or a cast of a constant is an expression.
//!
//! An integer there is a position in the select list as `*` expands it,
//! and one outside it is 42P10 `DISTINCT ON position N is not in select
//! list` at the constant, found in the same pass and the same order
//! (RP-884): `distinct on (9, 1.5)` is the position's error. A position
//! inside a star names the column the star stands for there, and is
//! written as that column ([`distinct_on_star_positions`]).

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

/// How many columns `s`'s select list has as `*` expands it, when that
/// is known.
fn select_list_width(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Option<usize> {
    if let Some(targets) = domain_scope::expanded_targets(s, lookup) {
        return Some(targets.len());
    }
    let star = s.target_list.iter().any(|t| {
        matches!(t.node.as_ref(), Some(N::ResTarget(rt))
            if matches!(rt.val.as_deref().and_then(|v| v.node.as_ref()),
                Some(N::ColumnRef(c)) if c.fields.last().is_some_and(|f| matches!(f.node, Some(N::AStar(_))))))
    });
    (!star).then_some(s.target_list.len())
}

/// The first DISTINCT ON item `findTargetlistEntrySQL92` refuses: a
/// non-integer constant, or a position outside the `width` columns of the
/// select list (one not checked when `width` is not known). Its location,
/// how many items come before it, and the position when it is one.
fn distinct_on_refused(items: &[Node], width: Option<usize>) -> Option<(i32, usize, Option<i32>)> {
    // `DISTINCT` alone is one item with no node.
    items
        .iter()
        .enumerate()
        .find_map(|(i, item)| match item.node.as_ref() {
            Some(N::AConst(c)) => match c.val.as_ref() {
                Some(a_const::Val::Ival(v)) => {
                    let outside = match width {
                        Some(width) => {
                            !usize::try_from(v.ival).is_ok_and(|pos| (1..=width).contains(&pos))
                        }
                        // Not known: only a position no list holds.
                        None => v.ival < 1,
                    };
                    outside.then_some((c.location, i, Some(v.ival)))
                }
                _ => Some((c.location, i, None)),
            },
            _ => None,
        })
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
    cut.distinct_clause.clear();
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

/// PostgreSQL's 42601 for a DISTINCT ON constant that is no integer, and
/// its 42P10 for a position outside the select list, unless the statement
/// fails earlier (module docs). Runs after [`check`], whose GROUP BY error
/// comes first.
pub(crate) fn check_distinct_on(
    s: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    plan: &dyn Fn(&SelectStmt) -> Result<Statement>,
) -> Result<()> {
    if s.distinct_clause.iter().all(|n| n.node.is_none()) {
        return Ok(());
    }
    let width = select_list_width(s, lookup);
    let Some((at, before, position)) = distinct_on_refused(&s.distinct_clause, width) else {
        return Ok(());
    };
    let mut cut = s.clone();
    cut.limit_count = None;
    cut.limit_offset = None;
    cut.locking_clause.clear();
    // The clauses analysed before DISTINCT ON, then its items before this
    // one. Planned with them, a statement can fail the ORDER BY match,
    // which is checked only after every item, ahead of an ORDER BY error
    // of its own.
    cut.distinct_clause.clear();
    let mut cuts = vec![cut.clone()];
    if before > 0 {
        cut.distinct_clause = s.distinct_clause[..before].to_vec();
        cuts.push(cut);
    }
    for cut in &cuts {
        if let Err(e) = plan(cut) {
            let unmatched = e.to_string().starts_with(
                "SELECT DISTINCT ON expressions must match initial ORDER BY expressions",
            );
            if !unmatched && raised_before_group_by(&e) {
                return Err(e);
            }
        }
    }
    set_error_location(at);
    Err(match position {
        Some(pos) => Error::InvalidColumnReference(format!(
            "DISTINCT ON position {pos} is not in select list"
        )),
        None => Error::Sqlstate("42601", "non-integer constant in DISTINCT ON".into()),
    })
}

/// `s` with each DISTINCT ON position that falls inside a `*` written as
/// the column the star stands for there, qualified by its FROM item:
/// `distinct on (2) * from t` sorts and compares by `t`'s second column
/// (`findTargetlistEntrySQL92` returns the star's expanded target).
/// `None` when there is none.
pub(crate) fn distinct_on_star_positions(
    s: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<SelectStmt> {
    let position = |n: &Node| match n.node.as_ref() {
        Some(N::AConst(c)) => match c.val.as_ref() {
            Some(a_const::Val::Ival(v)) => Some((usize::try_from(v.ival).ok()?, c.location)),
            _ => None,
        },
        _ => None,
    };
    if !s.distinct_clause.iter().any(|n| position(n).is_some()) {
        return None;
    }
    let targets = domain_scope::expanded_targets(s, lookup)?;
    let mut out = s.clone();
    let mut changed = false;
    for item in &mut out.distinct_clause {
        let Some((pos, location)) = position(item) else {
            continue;
        };
        let Some(Some((from_item, column))) = pos.checked_sub(1).and_then(|k| targets.get(k))
        else {
            continue;
        };
        *item = domain_scope::star_column_ref(from_item, column, location);
        changed = true;
    }
    changed.then_some(out)
}
