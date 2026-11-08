//! A column reference of three names, `schema.rel.col` (RP-527).
//!
//! PostgreSQL 15.19's `transformColumnRef` reads `schema.rel` as a
//! RELATION, not as a name: `refnameNamespaceItem` looks `rel` up in
//! `schema` (`get_relname_relid`; nothing when either is missing) and then
//! finds, innermost query level first, a namespace item that is that
//! relation and has NO alias (`scanNameSpaceForRelid`). When none is
//! found, `errorMissingRTE` searches the range tables, innermost first,
//! for an entry that is that relation or whose `eref` is `rel`
//! (`searchRangeTableForRel`), and raises 42P01 at the reference:
//!
//! - an aliased entry whose alias is visible from the reference is
//!   `invalid reference to FROM-clause entry for table "rel"`, HINT
//!   `Perhaps you meant to reference the table alias "x".`;
//! - any other entry is the same message with HINT `There is an entry for
//!   table "e", but it cannot be referenced from this part of the query.`
//!   -- an unaliased `fs` read as `other.fs` or `s.fs`, a write's target
//!   from an INSERT's SELECT, a table a non-LATERAL subquery cannot see;
//! - no entry is `missing FROM-clause entry for table "rel"`.
//!
//! The planner after this resolves a qualifier by its NAME, so `other.fs.c`
//! read `fs.c`. This pass runs first, on a statement as its client sent it
//! (a stored definition keeps reading what it was created over), and
//! models the query levels PostgreSQL builds: a SELECT's FROM (a join's ON
//! sees only its two sides, an aliased join hides them, a FROM subquery or
//! function sees the items before it only through LATERAL, which a
//! function has implicitly), a write's target (an INSERT's is in the range
//! table but not the namespace, save for RETURNING and ON CONFLICT, where
//! EXCLUDED is too), and a WITH's CTEs (a relation the name never is). A
//! reference that resolves is rewritten to the two names `rel.col` when
//! those find the same entry, which is what the lookup yields; otherwise it
//! is left for the planner. A FROM item naming no relation the session can
//! see stops the pass, so its own 42P01 comes from the planner as before.
//!
//! The clauses are read in PostgreSQL's transform order, so the first
//! failing reference is the one reported among those of three names.
//! References of two names keep the checks they have
//! (`check_subquery_qualifiers`, `check_write_qualifiers`,
//! `check_single_source_qualifiers`), and so does a body a function's
//! parameter hooks read (a SQL or PL/pgSQL function's), which is never
//! planned as sent.

use super::*;
use pg_query::protobuf as pb;

/// One range-table entry, as `searchRangeTableForRel` and
/// `scanNameSpaceForRelid` read it.
#[derive(Clone, Debug)]
struct Entry {
    /// The relation of an `RTE_RELATION` as written, `([schema], name)`;
    /// `None` for a CTE, a subquery, a function or a join. Its oid is
    /// looked up only when a reference of three names needs it.
    relation: Option<(Option<String>, String)>,
    /// `rte->eref->aliasname`: the alias, else the relation's own name.
    eref: String,
    /// `rte->alias != NULL`.
    aliased: bool,
    /// In the namespace the reference is resolved against (`p_rel_visible`,
    /// and not lateral-only where LATERAL is not active).
    visible: bool,
}

impl Entry {
    fn other(eref: String, aliased: bool, visible: bool) -> Entry {
        Entry {
            relation: None,
            eref,
            aliased,
            visible,
        }
    }
}

/// One query level: its range table in PostgreSQL's order, and the CTE
/// names its WITH defines.
#[derive(Clone, Debug, Default)]
struct Level {
    entries: Vec<Entry>,
    ctes: Vec<String>,
}

/// `Err(None)` stops the pass with nothing to report.
type Step = std::result::Result<(), Option<Error>>;

/// Check (and, where they resolve, rewrite) every three-name column
/// reference of a statement as sent.
pub(crate) fn check(node: &mut N) -> Result<()> {
    if !schemas::AS_SENT.with(std::cell::Cell::get) {
        return Ok(());
    }
    match statement(node, &[]) {
        Ok(()) | Err(None) => Ok(()),
        Err(Some(e)) => Err(e),
    }
}

fn three_names(c: &pb::ColumnRef) -> Option<(&str, &str)> {
    let [schema, rel, last] = c.fields.as_slice() else {
        return None;
    };
    let (Some(N::String(schema)), Some(N::String(rel))) = (schema.node.as_ref(), rel.node.as_ref())
    else {
        return None;
    };
    matches!(last.node.as_ref(), Some(N::String(_) | N::AStar(_)))
        .then_some((schema.sval.as_str(), rel.sval.as_str()))
}

fn statement(node: &mut N, outer: &[Level]) -> Step {
    match node {
        N::SelectStmt(s) => select(s, outer),
        N::InsertStmt(i) => insert(i, outer),
        N::UpdateStmt(u) => update(u, outer),
        N::DeleteStmt(d) => delete(d, outer),
        N::ExplainStmt(e) => query(e.query.as_deref_mut(), outer),
        N::ViewStmt(v) => query(v.query.as_deref_mut(), outer),
        N::CreateTableAsStmt(c) => query(c.query.as_deref_mut(), outer),
        N::DeclareCursorStmt(d) => query(d.query.as_deref_mut(), outer),
        N::PrepareStmt(p) => query(p.query.as_deref_mut(), outer),
        N::CopyStmt(c) => query(c.query.as_deref_mut(), outer),
        _ => Ok(()),
    }
}

fn query(node: Option<&mut pb::Node>, outer: &[Level]) -> Step {
    match node.and_then(|n| n.node.as_mut()) {
        Some(n) => statement(n, outer),
        None => Ok(()),
    }
}

fn with_level(outer: &[Level], level: &Level) -> Vec<Level> {
    let mut levels = outer.to_vec();
    levels.push(level.clone());
    levels
}

/// A WITH's CTEs, each body read with the enclosing levels and the CTE
/// names (`transformWithClause`); the names go on `level`.
fn with_clause(with: Option<&mut pb::WithClause>, outer: &[Level], level: &mut Level) -> Step {
    let Some(with) = with else {
        return Ok(());
    };
    for cte in &with.ctes {
        if let Some(N::CommonTableExpr(c)) = cte.node.as_ref() {
            level.ctes.push(c.ctename.clone());
        }
    }
    let levels = with_level(outer, level);
    for cte in &mut with.ctes {
        if let Some(N::CommonTableExpr(c)) = cte.node.as_mut() {
            query(c.ctequery.as_deref_mut(), &levels)?;
        }
    }
    Ok(())
}

fn is_cte(outer: &[Level], level: &Level, name: &str) -> bool {
    level
        .ctes
        .iter()
        .chain(outer.iter().flat_map(|l| &l.ctes))
        .any(|c| c == name)
}

/// The entry a `RangeVar` adds.
fn relation_entry(r: &pb::RangeVar, outer: &[Level], level: &Level) -> Entry {
    let cte = r.schemaname.is_empty() && is_cte(outer, level, &r.relname);
    Entry {
        relation: (!cte).then(|| {
            (
                (!r.schemaname.is_empty()).then(|| r.schemaname.clone()),
                r.relname.clone(),
            )
        }),
        eref: r
            .alias
            .as_ref()
            .map_or_else(|| r.relname.clone(), |a| a.aliasname.clone()),
        aliased: r.alias.is_some(),
        visible: true,
    }
}

fn select(s: &mut pb::SelectStmt, outer: &[Level]) -> Step {
    let mut level = Level::default();
    with_clause(s.with_clause.as_mut(), outer, &mut level)?;
    if s.larg.is_some() || s.rarg.is_some() {
        // Each arm is a query of its own, in the WITH's scope.
        let levels = with_level(outer, &level);
        for arm in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
            .into_iter()
            .flatten()
        {
            select(arm, &levels)?;
        }
        return Ok(());
    }
    if !s.values_lists.is_empty() {
        let levels = with_level(outer, &level);
        return clauses(s.values_lists.iter_mut(), &levels);
    }
    for item in &mut s.from_clause {
        from_item(item, outer, &mut level)?;
    }
    let levels = with_level(outer, &level);
    // `transformSelectStmt`'s order.
    clauses(s.target_list.iter_mut(), &levels)?;
    clauses(s.where_clause.as_deref_mut().into_iter(), &levels)?;
    clauses(s.having_clause.as_deref_mut().into_iter(), &levels)?;
    clauses(s.sort_clause.iter_mut(), &levels)?;
    clauses(s.group_clause.iter_mut(), &levels)?;
    clauses(s.distinct_clause.iter_mut(), &levels)?;
    clauses(
        s.limit_count
            .as_deref_mut()
            .into_iter()
            .chain(s.limit_offset.as_deref_mut()),
        &levels,
    )?;
    clauses(s.window_clause.iter_mut(), &levels)
}

/// The levels a FROM item's own expressions see: the enclosing ones, then
/// this one so far, whose items are visible only through LATERAL.
fn lateral_levels(outer: &[Level], level: &Level, lateral: bool) -> Vec<Level> {
    let mut so_far = level.clone();
    if !lateral {
        for e in &mut so_far.entries {
            e.visible = false;
        }
    }
    with_level(outer, &so_far)
}

/// Add one FROM item's entries to `level` (`transformFromClauseItem`),
/// reading its expressions on the way.
fn from_item(item: &mut pb::Node, outer: &[Level], level: &mut Level) -> Step {
    match item.node.as_mut() {
        Some(N::RangeVar(r)) => {
            let e = relation_entry(r, outer, level);
            level.entries.push(e);
        }
        Some(N::RangeTableSample(ts)) => {
            let Some(N::RangeVar(r)) = ts.relation.as_deref().and_then(|r| r.node.as_ref()) else {
                return Err(None);
            };
            let e = relation_entry(r, outer, level);
            level.entries.push(e);
            let levels = with_level(outer, level);
            clauses(
                ts.args.iter_mut().chain(ts.repeatable.as_deref_mut()),
                &levels,
            )?;
        }
        Some(N::RangeSubselect(rs)) => {
            let levels = lateral_levels(outer, level, rs.lateral);
            query(rs.subquery.as_deref_mut(), &levels)?;
            let Some(alias) = rs.alias.as_ref() else {
                return Err(None);
            };
            level
                .entries
                .push(Entry::other(alias.aliasname.clone(), true, true));
        }
        Some(N::RangeFunction(rf)) => {
            // A function in FROM is LATERAL whether or not it says so.
            let levels = lateral_levels(outer, level, true);
            let mut first_name = None;
            for f in &mut rf.functions {
                if let Some(N::List(l)) = f.node.as_mut() {
                    if let Some(N::FuncCall(fc)) = l.items.first().and_then(|c| c.node.as_ref()) {
                        if first_name.is_none() {
                            first_name = fc.funcname.last().and_then(|n| match n.node.as_ref() {
                                Some(N::String(s)) => Some(s.sval.clone()),
                                _ => None,
                            });
                        }
                    }
                    clauses(l.items.iter_mut().take(1), &levels)?;
                }
            }
            let eref = rf
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .or(first_name)
                .unwrap_or_default();
            level
                .entries
                .push(Entry::other(eref, rf.alias.is_some(), true));
        }
        Some(N::JoinExpr(j)) => {
            let start = level.entries.len();
            for side in [j.larg.as_deref_mut(), j.rarg.as_deref_mut()]
                .into_iter()
                .flatten()
            {
                from_item(side, outer, level)?;
            }
            let end = level.entries.len();
            // The ON clause sees the join's two sides alone
            // (`transformJoinOnClause`).
            let mut on = level.clone();
            for (i, e) in on.entries.iter_mut().enumerate() {
                e.visible &= (start..end).contains(&i);
            }
            clauses(j.quals.as_deref_mut().into_iter(), &with_level(outer, &on))?;
            // An aliased join hides its sides; an unaliased one is not
            // visible itself.
            let aliased = j.alias.is_some();
            if aliased {
                for e in &mut level.entries[start..end] {
                    e.visible = false;
                }
            }
            let eref = j
                .alias
                .as_ref()
                .map_or_else(|| "unnamed_join".to_string(), |a| a.aliasname.clone());
            level.entries.push(Entry::other(eref, aliased, aliased));
        }
        _ => return Err(None),
    }
    Ok(())
}

/// A write's target (`setTargetTable`), first in its range table.
fn target_entry(
    r: Option<&pb::RangeVar>,
    outer: &[Level],
    level: &Level,
    visible: bool,
) -> std::result::Result<Entry, Option<Error>> {
    let mut e = relation_entry(r.ok_or(None)?, outer, level);
    e.visible = visible;
    Ok(e)
}

fn insert(i: &mut pb::InsertStmt, outer: &[Level]) -> Step {
    let mut level = Level::default();
    with_clause(i.with_clause.as_mut(), outer, &mut level)?;
    // The target is in the range table, not the namespace, while the
    // source is read (`transformInsertStmt`).
    let target = target_entry(i.relation.as_ref(), outer, &level, false)?;
    level.entries.push(target.clone());
    query(i.select_stmt.as_deref_mut(), &with_level(outer, &level))?;
    level.entries[0].visible = true;
    if let Some(oc) = i.on_conflict_clause.as_deref_mut() {
        // EXCLUDED is the target relation under that alias
        // (`transformOnConflictClause`), in the namespace for this clause
        // alone.
        let excluded = Entry {
            eref: "excluded".into(),
            aliased: true,
            ..target
        };
        let mut conflict = level.clone();
        conflict.entries.push(excluded.clone());
        let levels = with_level(outer, &conflict);
        clauses(oc.target_list.iter_mut(), &levels)?;
        clauses(oc.where_clause.as_deref_mut().into_iter(), &levels)?;
        level.entries.push(Entry {
            visible: false,
            ..excluded
        });
    }
    clauses(i.returning_list.iter_mut(), &with_level(outer, &level))
}

fn update(u: &mut pb::UpdateStmt, outer: &[Level]) -> Step {
    let mut level = Level::default();
    with_clause(u.with_clause.as_mut(), outer, &mut level)?;
    let target = target_entry(u.relation.as_ref(), outer, &level, true)?;
    level.entries.push(target);
    for item in &mut u.from_clause {
        from_item(item, outer, &mut level)?;
    }
    let levels = with_level(outer, &level);
    // `transformUpdateStmt`: WHERE, RETURNING, then SET.
    clauses(u.where_clause.as_deref_mut().into_iter(), &levels)?;
    clauses(u.returning_list.iter_mut(), &levels)?;
    clauses(u.target_list.iter_mut(), &levels)
}

fn delete(d: &mut pb::DeleteStmt, outer: &[Level]) -> Step {
    let mut level = Level::default();
    with_clause(d.with_clause.as_mut(), outer, &mut level)?;
    let target = target_entry(d.relation.as_ref(), outer, &level, true)?;
    level.entries.push(target);
    for item in &mut d.using_clause {
        from_item(item, outer, &mut level)?;
    }
    let levels = with_level(outer, &level);
    clauses(d.where_clause.as_deref_mut().into_iter(), &levels)?;
    clauses(d.returning_list.iter_mut(), &levels)
}

/// Read each clause's expressions; a subquery's body is its own level.
fn clauses<'a>(nodes: impl Iterator<Item = &'a mut pb::Node>, levels: &[Level]) -> Step {
    for n in nodes {
        let mut stop: Option<Option<Error>> = None;
        let walked = walk_expr(n, &mut |n| {
            if stop.is_some() {
                return Ok(());
            }
            let outcome = match n.node.as_mut() {
                Some(N::ColumnRef(c)) => column_ref(c, levels),
                Some(N::SubLink(sl)) => query(sl.subselect.as_deref_mut(), levels),
                _ => Ok(()),
            };
            if let Err(e) = outcome {
                stop = Some(e);
            }
            Ok(())
        });
        if let Some(e) = stop {
            return Err(e);
        }
        walked.map_err(Some)?;
    }
    Ok(())
}

/// `refnameNamespaceItem` for `schema.rel`, then `errorMissingRTE`.
fn column_ref(c: &mut pb::ColumnRef, levels: &[Level]) -> Step {
    let Some((schema, rel)) = three_names(c) else {
        return Ok(());
    };
    let relid = relation_oid(Some(schema), rel);
    let rel = rel.to_string();
    // Every relation in reach, by oid. One that names nothing is an error
    // PostgreSQL raised at its FROM item, before this reference was read:
    // the planner reports it.
    let mut oids: Vec<Vec<Option<i64>>> = Vec::with_capacity(levels.len());
    for l in levels {
        let mut level = Vec::with_capacity(l.entries.len());
        for e in &l.entries {
            level.push(match e.relation.as_ref() {
                Some((schema, name)) => Some(relation_oid(schema.as_deref(), name).ok_or(None)?),
                None => None,
            });
        }
        oids.push(level);
    }
    let is_relid = |li: usize, ei: usize| relid.is_some() && oids[li][ei] == relid;
    // The innermost entry `pred` holds for: (level, entry).
    let innermost = |pred: &dyn Fn(usize, usize, &Entry) -> bool| {
        levels.iter().enumerate().rev().find_map(|(li, l)| {
            l.entries
                .iter()
                .enumerate()
                .find(|(ei, e)| pred(li, *ei, e))
                .map(|(ei, _)| (li, ei))
        })
    };
    // As a two-name reference finds an `eref`.
    let by_name = |name: &str| innermost(&|_, _, e| e.visible && e.eref == name);
    if let Some(found) = innermost(&|li, ei, e| e.visible && !e.aliased && is_relid(li, ei)) {
        // Read as the entry's own name when that finds the same entry.
        if by_name(&rel) == Some(found) {
            c.fields.remove(0);
        }
        return Ok(());
    }
    set_error_location(c.location);
    let message = match innermost(&|li, ei, e| is_relid(li, ei) || e.eref == rel) {
        Some((li, ei)) => {
            let e = &levels[li].entries[ei];
            if e.aliased && e.eref != rel && by_name(&e.eref) == Some((li, ei)) {
                format!(
                    "invalid reference to FROM-clause entry for table \"{rel}\"\nHint: Perhaps you meant to reference the table alias \"{}\".",
                    e.eref
                )
            } else {
                format!(
                    "invalid reference to FROM-clause entry for table \"{rel}\"\nHint: There is an entry for table \"{}\", but it cannot be referenced from this part of the query.",
                    e.eref
                )
            }
        }
        None => format!("missing FROM-clause entry for table \"{rel}\""),
    };
    Err(Some(Error::Sqlstate("42P01", message)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `check` of `sql` as sent, with `fs` and `fe` in `public` and `fs` in
    /// the schema `s`: the statement deparsed, or the 42P01 message.
    fn outcome(sql: &str) -> std::result::Result<String, String> {
        crate::set_user_relations(vec![
            ("fs".into(), 16384, false),
            ("fe".into(), 16387, false),
            ("s.fs".into(), 16390, false),
        ]);
        schemas::set_user_schemas(vec!["s".into()]);
        let previous = schemas::AS_SENT.with(|f| f.replace(true));
        let mut parsed = pg_query::parse(sql).expect("parses").protobuf;
        let stmt = parsed.stmts[0].stmt.as_mut().expect("a statement");
        let checked = check(stmt.node.as_mut().expect("a node"));
        schemas::AS_SENT.with(|f| f.set(previous));
        match checked {
            Ok(()) => Ok(pg_query::deparse(&parsed).expect("deparses")),
            Err(Error::Sqlstate("42P01", m)) => Err(m),
            Err(e) => panic!("{sql}: {e:?}"),
        }
    }

    const ENTRY: &str = "invalid reference to FROM-clause entry for table \"fs\"\nHint: There is an entry for table \"fs\", but it cannot be referenced from this part of the query.";

    #[test]
    fn a_schema_qualified_reference_names_an_unaliased_relation() {
        // PostgreSQL 15.19 (RP-527): the schema is read, the reference finds
        // the relation only where it has no alias, and one that resolves
        // reads as the entry's own name.
        for (sql, read) in [
            ("select public.fs.c from fs", "SELECT fs.c FROM fs"),
            ("select public.fs.* from fs", "SELECT fs.* FROM fs"),
            ("select s.fs.c from s.fs", "SELECT fs.c FROM s.fs"),
            (
                "select (select public.fs.c from fe) from fs",
                "SELECT (SELECT fs.c FROM fe) FROM fs",
            ),
            (
                "select x.a from fs, lateral (select public.fs.a) x",
                "SELECT x.a FROM fs, LATERAL (SELECT fs.a) x",
            ),
        ] {
            assert_eq!(outcome(sql).as_deref(), Ok(read), "{sql}");
        }
        for sql in [
            "select other.fs.c from fs",
            "select s.fs.c from fs",
            "select public.fs.c from s.fs",
            "select * from fs, (select public.fs.c) x",
            "select * from (fs join fe using (a)) j where public.fs.a = 1",
            "with fs as (select 1 as c) select public.fs.c from fs",
            "insert into fs select public.fs.a, 'z' from fe",
            "insert into fs values (3, 'q') returning other.fs.a",
            "delete from fs where other.fs.a = 1",
            "update fs set c = other.fs.c",
        ] {
            assert_eq!(outcome(sql), Err(ENTRY.to_string()), "{sql}");
        }
        assert_eq!(
            outcome("select public.fs.c from fs x"),
            Err("invalid reference to FROM-clause entry for table \"fs\"\nHint: Perhaps you meant to reference the table alias \"x\".".to_string())
        );
        assert_eq!(
            outcome("select pg_catalog.fs.c from fs x"),
            Err("missing FROM-clause entry for table \"fs\"".to_string())
        );
    }

    #[test]
    fn a_relation_that_does_not_exist_is_left_to_the_planner() {
        // `relation "nosuch" does not exist` is raised at the FROM item,
        // before any column reference is read.
        assert_eq!(
            outcome("select public.fs.c from nosuch").as_deref(),
            Ok("SELECT public.fs.c FROM nosuch")
        );
    }
}
