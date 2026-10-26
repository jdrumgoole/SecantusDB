//! General JOINs: a FROM list holding a `JoinExpr`, or more than one item.
//!
//! A join is planned as a SOURCE -- a tree of materialised leaves combined by
//! the executor -- and everything after that is the ordinary single-source
//! planner: WHERE, GROUP BY, HAVING, windows, DISTINCT, ORDER BY and LIMIT all
//! work on documents and never learn the rows came from two tables. That is
//! the same move that gave FROM-subqueries and set-returning functions every
//! clause for free.
//!
//! The one thing a join adds is NAMES. Two sides may both have an `id`, and
//! this planner's column lowering resolves a reference by its LAST name part,
//! ignoring the qualifier -- so `j.id` and `k.id` would be one column. The
//! joined rows are therefore keyed `alias<SEP>column`, and every column
//! reference in the query is rewritten to that key before planning. The
//! source exposes ONLY those keys: a reference the rewrite did not reach
//! resolves by its bare name, finds nothing, and is a 42703 -- never a silent
//! binding to the wrong side.

use super::*;

/// The separator inside a joined row's keys. A unit separator: no identifier
/// carries one, and unlike `.` an MQL filter does not read it as a path.
pub const SEP: char = '\u{1f}';

/// The key a side's column is stored under in a joined row.
pub fn join_key(alias: &str, col: &str) -> String {
    format!("{alias}{SEP}{col}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
}

/// A planned join tree. Every leaf is a planned SELECT (a table is `SELECT *
/// FROM t`), so values arrive decoded exactly as any query's do.
#[derive(Debug, Clone, PartialEq)]
pub enum JoinNode {
    Leaf {
        plan: Box<Statement>,
        /// The leaf's output def, whose fields the materialised rows carry.
        def: TableDef,
        /// `(joined-row key, field in the leaf's rows)`.
        columns: Vec<(String, String)>,
    },
    /// A LATERAL item: SQL re-run for every row of the join's LEFT side, its
    /// references to that side bound as `$N` parameters past the statement's
    /// own. Only ever the right side of a `Join`.
    Lateral {
        sql: String,
        /// The statement's own parameters, which the SQL's `$1..$n` still are.
        params: Vec<Bson>,
        /// The left side's keys whose values fill `$n+1..`, in order.
        keys: Vec<String>,
        /// `(joined-row key, position in the SQL's select list)`.
        columns: Vec<(String, usize)>,
    },
    Join {
        kind: JoinKind,
        left: Box<JoinNode>,
        right: Box<JoinNode>,
        /// The ON predicate over the combined row; `None` is a cross join.
        on: Option<ColumnExpr>,
        /// The table aliases the ON reads, in no particular order.
        on_aliases: Vec<String>,
        /// Whether the ON holds a conjunct that is not one of its
        /// equalities (`equi`, `expr_keys`): a FULL join's merge or hash
        /// then checks it as a join filter, which is not modelled.
        on_other: bool,
        /// `(left key, right key)` column equalities that are conjuncts of
        /// the ON: candidates are found by hashing on them, and `on` still
        /// decides each pair.
        equi: Vec<(String, String)>,
        /// For each `equi` pair, whether the ON wrote it right key first
        /// (`r.x = l.y`): the order PostgreSQL's `process_equivalence` adds
        /// the two columns to an equivalence class, which decides the
        /// members a derived join clause takes.
        equi_reversed: Vec<bool>,
        /// USING / NATURAL columns: `(merged key, left key, right key)`.
        merged: Vec<(String, String, String)>,
        /// ON conjuncts equating an expression over one side with one over
        /// the other (`a.x = b.y + 3`), which are a merge or hash join's
        /// keys as `equi`'s columns are; `on` still decides each pair.
        expr_keys: Vec<ExprEquality>,
        /// What PostgreSQL's equivalence classes make of the ON's and the
        /// WHERE's equalities, for a join of two tables (`JoinEc`); `None`
        /// when they were not derived.
        ec: Option<JoinEc>,
        /// Every key each side produces, so an outer join can NULL-extend.
        left_keys: Vec<String>,
        right_keys: Vec<String>,
    },
}

/// One `left expression = right expression` conjunct of an ON
/// (`JoinNode::Join::expr_keys`), at least one side not a bare column.
/// Each side's expression reads only that side's keys; `*_ops` is how many
/// operators it evaluates, which the join clause's cost charges
/// (`cost_qual_eval`).
#[derive(Debug, Clone, PartialEq)]
pub struct ExprEquality {
    pub left: ColumnExpr,
    pub right: ColumnExpr,
    pub left_ops: u32,
    pub right_ops: u32,
    /// The joined-row key a side's operand is, when it is a bare column.
    pub left_column: Option<String>,
    pub right_column: Option<String>,
}

/// The equalities of a two-table join's ON and WHERE as PostgreSQL's
/// equivalence classes leave them (`generate_base_implied_equalities`,
/// `generate_join_implied_equalities`), as joined-row keys. A class with a
/// constant becomes `column = constant` on every member, pushed into each
/// side's scan, and joins nothing; one without becomes a chain of
/// equalities between each side's own members, and one join clause, its
/// first member on each side.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct JoinEc {
    /// Equalities between two columns of one side.
    pub left_restrict: Vec<(String, String)>,
    pub right_restrict: Vec<(String, String)>,
    /// The join clauses kept, `(left key, right key)`: none when every
    /// class has a constant, and the join is a nested loop over all pairs.
    pub join_keys: Vec<(String, String)>,
}

/// `JoinEc` before the join is built: by table alias and column.
#[derive(Debug, Clone, Default)]
struct EcPlan {
    aliases: Vec<String>,
    restrict: Vec<(String, String, String)>,
    join_keys: Vec<((String, String), (String, String))>,
}

impl EcPlan {
    /// The `JoinEc` for a join of `left` (its keys) and `right`, when they
    /// are this plan's two tables.
    fn for_join(&self, left: &[String], right: &[String]) -> Option<JoinEc> {
        let side_of = |alias: &str| -> Option<bool> {
            let probe = |keys: &[String]| keys.iter().any(|k| k.split(SEP).next() == Some(alias));
            match (probe(left), probe(right)) {
                (true, false) => Some(true),
                (false, true) => Some(false),
                _ => None,
            }
        };
        let mut sides = Vec::new();
        for a in &self.aliases {
            sides.push(side_of(a)?);
        }
        if sides.iter().all(|s| *s) || sides.iter().all(|s| !*s) {
            return None;
        }
        let mut ec = JoinEc::default();
        for (alias, a, b) in &self.restrict {
            let pair = (join_key(alias, a), join_key(alias, b));
            if side_of(alias)? {
                ec.left_restrict.push(pair);
            } else {
                ec.right_restrict.push(pair);
            }
        }
        for ((xa, xc), (ya, yc)) in &self.join_keys {
            let (x, y) = (join_key(xa, xc), join_key(ya, yc));
            ec.join_keys
                .push(if side_of(xa)? { (x, y) } else { (y, x) });
        }
        Some(ec)
    }
}

/// A join as a statement the executor materialises: its tree and the def of
/// the rows it produces.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinRows {
    pub tree: JoinNode,
    pub def: TableDef,
    /// One of the query's WHERE conjuncts over more than one table is an
    /// equality, which PostgreSQL would put in an equivalence class and so
    /// join on; the others it applies as join filters, which leave the join
    /// order's choices alone where they are first evaluated at the top join.
    pub where_spans_equality: bool,
    /// The tables (aliases) each of those conjuncts reads.
    pub where_span_aliases: Vec<Vec<String>>,
    /// Those conjuncts as row expressions over the joined keys, for the
    /// ones the search can place.
    pub where_filters: Vec<JoinFilter>,
    /// Those that are an equality of two tables' columns, as joined-key
    /// pairs: each a join clause between its two tables.
    pub where_equalities: Vec<(String, String)>,
    /// Every WHERE conjunct is evaluated at or below the join: pushed into
    /// the scan of the one table it reads, folded into an equivalence
    /// class, or reading two or more tables (whose join filters and
    /// clauses the search places). Only then do the join's rows, and its
    /// planner's estimate of them, number what the WHERE keeps, as a LIMIT
    /// over the join counts them (`grouping_planner`'s `tuple_fraction`).
    pub where_modelled: bool,
    /// The joined-row keys the query reads above the join -- its select
    /// list, GROUP BY, HAVING, ORDER BY and the rest, not its WHERE, whose
    /// conjuncts each read one table and run at its scan. They make the
    /// width of the rows a join passes up (`build_joinrel_tlist`).
    pub needed: Vec<String>,
}

/// One column visible in a join's scope.
#[derive(Debug, Clone)]
struct ScopeCol {
    alias: String,
    name: String,
    key: String,
    pg_type: String,
    /// Reachable by its bare name. False for a side's copy of a USING
    /// column, which only the merged column answers to unqualified.
    bare: bool,
    /// Part of `SELECT *`.
    star: bool,
    /// The base column it reads, for the RowDescription (RP-087).
    source: Option<(i64, i16)>,
}

#[derive(Debug, Clone, Default)]
struct Scope {
    cols: Vec<ScopeCol>,
    aliases: Vec<String>,
}

impl Scope {
    fn fields(&self) -> Vec<RowField> {
        self.cols
            .iter()
            .map(|c| (c.key.clone(), c.key.clone(), c.pg_type.clone()))
            .collect()
    }

    /// Resolve name parts to a key: `Ok(None)` when this scope does not know
    /// the name at all (it may belong to an output alias).
    fn resolve(&self, parts: &[String]) -> Result<Option<String>> {
        let (alias, name) = match parts {
            [name] => (None, name),
            [alias, name] => (Some(alias), name),
            [_, alias, name] => (Some(alias), name),
            _ => return Ok(None),
        };
        match alias {
            None => {
                let hits: Vec<&ScopeCol> = self
                    .cols
                    .iter()
                    .filter(|c| c.bare && &c.name == name)
                    .collect();
                match hits.as_slice() {
                    [] => Ok(None),
                    [one] => Ok(Some(one.key.clone())),
                    _ => Err(Error::Sqlstate(
                        "42702",
                        format!("column reference \"{name}\" is ambiguous"),
                    )),
                }
            }
            Some(alias) => {
                if !self.aliases.contains(alias) {
                    return Err(Error::Sqlstate(
                        "42P01",
                        format!("missing FROM-clause entry for table \"{alias}\""),
                    ));
                }
                self.cols
                    .iter()
                    .find(|c| &c.alias == alias && &c.name == name)
                    .map(|c| Some(c.key.clone()))
                    .ok_or_else(|| {
                        Error::Sqlstate("42703", format!("column {alias}.{name} does not exist"))
                    })
            }
        }
    }
}

thread_local! {
    /// Set while `build` plans a FROM function the query gave a bare alias
    /// (`f(...) AS x`), which names a ONE-column function's column; without
    /// an alias the column keeps its own name (`pg_partition_ancestors`'s
    /// `relid`), and a wider function's columns always do.
    static BARE_FUNCTION_ALIAS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// WHERE conjuncts pushed into a table leaf, by the leaf's alias, while
    /// its join is built (see `pushdown_filters`).
    static PUSHDOWN: std::cell::RefCell<Vec<(String, pg_query::protobuf::Node)>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The aliases the WHERE of the join being built proves non-NULL (see
    /// `pushdown_filters`): a LEFT join whose nullable side holds one is an
    /// inner join (`reduce_outer_joins`).
    static NONNULLABLE: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// The equivalence classes of the two-table join being built (see
    /// `equivalence_classes`).
    static EC_PLAN: std::cell::RefCell<Option<EcPlan>> = const { std::cell::RefCell::new(None) };
    /// Join sources planned for the statement in flight, by placeholder name.
    static PLANNED_JOINS: std::cell::RefCell<Vec<(String, SubSource)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The planned join a placeholder FROM item names, if it is one.
pub(crate) fn planned_join(name: &str) -> Option<SubSource> {
    if !name.starts_with(SEP) {
        return None;
    }
    PLANNED_JOINS.with(|j| {
        j.borrow()
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s.clone())
    })
}

/// Does this SELECT's FROM need the general join planner?
pub(crate) fn is_join(s: &pg_query::protobuf::SelectStmt) -> bool {
    s.from_clause.len() > 1
        || matches!(
            s.from_clause.first().and_then(|f| f.node.as_ref()),
            Some(N::JoinExpr(_))
        )
}

/// Plan the join in `s`'s FROM as a source, and return `s` rewritten to read
/// from it: its FROM a single placeholder, every column reference a key.
pub(crate) fn plan_join_source(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<pg_query::protobuf::SelectStmt> {
    // A nested join (one inside a FROM subquery) plans with its own.
    let (pushdown, nonnullable, ec, placed, moved) = pushdown_filters(s, lookup);
    let outer_pushdown = PUSHDOWN.with(|p| p.replace(pushdown));
    let outer_nonnullable = NONNULLABLE.with(|p| p.replace(nonnullable));
    let outer_ec = EC_PLAN.with(|p| p.replace(ec));
    let built = build_tree(s, lookup, params);
    PUSHDOWN.with(|p| *p.borrow_mut() = outer_pushdown);
    NONNULLABLE.with(|p| *p.borrow_mut() = outer_nonnullable);
    EC_PLAN.with(|p| *p.borrow_mut() = outer_ec);
    let (tree, scope) = built?;
    if moved.is_empty() {
        return finish_join_source(s, tree, scope, params, &placed);
    }
    // The moved conjuncts ran in their leaves; the WHERE keeps the rest,
    // and `placed` the rest's marks, in the same order.
    let mut rest = s.clone();
    rest.where_clause = s.where_clause.as_deref().and_then(|w| {
        let mut conjuncts = Vec::new();
        split_and(w, &mut conjuncts);
        let kept: Vec<pg_query::protobuf::Node> = conjuncts
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !moved.contains(i))
            .map(|(_, c)| c.clone())
            .collect();
        match kept.len() {
            0 => None,
            1 => kept.into_iter().next().map(Box::new),
            _ => Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                    boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                    args: kept,
                    location: -1,
                    ..Default::default()
                }))),
            })),
        }
    });
    let placed: Vec<bool> = placed
        .iter()
        .enumerate()
        .filter(|(i, _)| !moved.contains(i))
        .map(|(_, p)| *p)
        .collect();
    finish_join_source(&rest, tree, scope, params, &placed)
}

fn build_tree(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<(JoinNode, Scope)> {
    let mut scope = Scope::default();
    let mut items: Vec<(JoinNode, Vec<ScopeCol>)> = Vec::new();
    for item in &s.from_clause {
        let left = (!items.is_empty()).then(|| scope.clone());
        let (node, item_scope) = build(item, lookup, params, &mut scope.aliases, left.as_ref())?;
        scope.cols.extend(item_scope.cols.iter().cloned());
        items.push((node, item_scope.cols));
    }
    if items.is_empty() {
        return Err(Error::Parse("empty FROM".into()));
    }
    // `FROM a, b, c`: cross joins, combined in the order that lets each
    // join hash on a WHERE equality to what is already joined (see
    // `join_order`); left to right when nothing connects them.
    let lateral = items
        .iter()
        .any(|(n, _)| matches!(n, JoinNode::Lateral { .. }));
    let equalities = if lateral {
        Vec::new()
    } else {
        let cols: Vec<Vec<(String, String, String, String)>> = items
            .iter()
            .map(|(_, cols)| {
                cols.iter()
                    .map(|c| {
                        (
                            c.alias.clone(),
                            c.name.clone(),
                            c.key.clone(),
                            if c.bare {
                                c.pg_type.clone()
                            } else {
                                String::new()
                            },
                        )
                    })
                    .collect()
            })
            .collect();
        crate::join_order::where_equalities(s.where_clause.as_deref(), &cols)
    };
    let order = crate::join_order::order(items.len(), &equalities);
    let mut slots: Vec<Option<JoinNode>> = items.into_iter().map(|(n, _)| Some(n)).collect();
    let mut joined: Vec<usize> = Vec::new();
    let mut tree: Option<JoinNode> = None;
    for i in order {
        let node = slots[i].take().expect("each item joined once");
        tree = Some(match tree {
            None => node,
            Some(left) => JoinNode::Join {
                ec: EC_PLAN.with(|p| {
                    p.borrow()
                        .as_ref()
                        .and_then(|plan| plan.for_join(&keys_of(&left), &keys_of(&node)))
                }),
                kind: JoinKind::Inner,
                left_keys: keys_of(&left),
                right_keys: keys_of(&node),
                left: Box::new(left),
                right: Box::new(node),
                on: None,
                on_aliases: Vec::new(),
                on_other: false,
                // The WHERE still decides every row; these only narrow the
                // candidates a left row is paired with.
                equi: crate::join_order::pairs_between(&equalities, &joined, i),
                equi_reversed: crate::join_order::pairs_reversed(&equalities, &joined, i),
                merged: Vec::new(),
                expr_keys: Vec::new(),
            },
        });
        joined.push(i);
    }
    let tree = tree.expect("a non-empty FROM");
    Ok((tree, scope))
}

fn finish_join_source(
    s: &pg_query::protobuf::SelectStmt,
    tree: JoinNode,
    scope: Scope,
    params: &[Bson],
    placed: &[bool],
) -> Result<pg_query::protobuf::SelectStmt> {
    let spanning: Vec<&pg_query::protobuf::Node> = match s.where_clause.as_deref() {
        Some(w) => {
            let mut conjuncts = Vec::new();
            split_and(w, &mut conjuncts);
            conjuncts
                .into_iter()
                .filter(|c| single_alias(c, &|_| None).is_none())
                .collect()
        }
        None => Vec::new(),
    };
    // Each spanning conjunct's tables (by qualified column reference).
    let where_span_aliases: Vec<Vec<String>> = spanning
        .iter()
        .map(|c| {
            let mut out: Vec<String> = Vec::new();
            let mut node = (*c).clone();
            let _ = walk_expr(&mut node, &mut |x| {
                if let Some(N::ColumnRef(cr)) = x.node.as_ref() {
                    if let [q, _] = cr.fields.as_slice() {
                        if let Some(N::String(q)) = q.node.as_ref() {
                            if !out.contains(&q.sval) {
                                out.push(q.sval.clone());
                            }
                        }
                    }
                }
                Ok(())
            });
            out
        })
        .collect();
    let where_spans_equality = spanning.iter().any(|c| {
        matches!(c.node.as_ref(), Some(N::AExpr(e))
            if e.kind == pg_query::protobuf::AExprKind::AexprOp as i32
                && operator_name(e) == Ok("="))
    });
    let def = TableDef::new(
        "",
        scope
            .cols
            .iter()
            .map(|c| {
                let mut col = Column::new(&c.key, &c.pg_type, false);
                col.source = c.source;
                col
            })
            .collect(),
    );
    let mut out = s.clone();
    rewrite_query(&mut out, &scope)?;
    // The WHERE's conjuncts over more than one table, as row expressions over
    // the joined keys, for the join order search to apply where PostgreSQL
    // evaluates each: as a join filter of the first join holding its tables.
    // Its equalities between two tables' columns, as joined-key pairs: each
    // an equivalence class PostgreSQL joins on, a clause between them.
    let where_equalities: Vec<(String, String)> = match out.where_clause.as_deref() {
        Some(w) => {
            let mut conjuncts = Vec::new();
            split_and(w, &mut conjuncts);
            conjuncts
                .into_iter()
                .filter_map(|c| {
                    let Some(N::AExpr(e)) = c.node.as_ref() else {
                        return None;
                    };
                    if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32
                        || operator_name(e) != Ok("=")
                    {
                        return None;
                    }
                    let key = |x: Option<&pg_query::protobuf::Node>| match x?.node.as_ref()? {
                        N::ColumnRef(c) if c.fields.len() == 1 => column_ref_name(c),
                        _ => None,
                    };
                    let (l, r) = (key(e.lexpr.as_deref())?, key(e.rexpr.as_deref())?);
                    let alias = |k: &str| k.split_once(SEP).map(|(a, _)| a.to_string());
                    (alias(&l)? != alias(&r)?).then_some((l, r))
                })
                .collect()
        }
        None => Vec::new(),
    };
    let where_filters: Vec<JoinFilter> = match out.where_clause.as_deref() {
        Some(w) => {
            let mut conjuncts = Vec::new();
            split_and(w, &mut conjuncts);
            conjuncts
                .into_iter()
                .filter_map(|c| {
                    let aliases = key_aliases(c);
                    if aliases.len() < 2 {
                        return None;
                    }
                    let kind = filter_kind(c);
                    let ops = operator_count(c);
                    let expr = on_expr(c, &scope, params).ok()?;
                    Some(JoinFilter {
                        aliases,
                        expr,
                        kind,
                        ops,
                    })
                })
                .collect()
        }
        None => Vec::new(),
    };
    // Each conjunct pushed or folded (`placed`, in WHERE order), or over
    // two or more tables.
    let where_modelled = match out.where_clause.as_deref() {
        Some(w) => {
            let mut conjuncts = Vec::new();
            split_and(w, &mut conjuncts);
            conjuncts.len() == placed.len()
                && conjuncts
                    .iter()
                    .zip(placed)
                    .all(|(c, p)| *p || key_aliases(c).len() >= 2)
        }
        None => true,
    };
    let needed = {
        let mut above = out.clone();
        above.where_clause = None;
        above.from_clause.clear();
        let node = N::SelectStmt(Box::new(above));
        let mut keys: Vec<String> = Vec::new();
        for (n, _, _, _) in node.nodes() {
            if let pg_query::NodeRef::ColumnRef(c) = n {
                if let [f] = c.fields.as_slice() {
                    if let Some(N::String(k)) = f.node.as_ref() {
                        if k.sval.contains(SEP) && !keys.contains(&k.sval) {
                            keys.push(k.sval.clone());
                        }
                    }
                }
            }
        }
        keys
    };
    let name = PLANNED_JOINS.with(|j| {
        let mut j = j.borrow_mut();
        let name = format!("{SEP}join{SEP}{}", j.len());
        j.push((
            name.clone(),
            SubSource {
                alias: String::new(),
                plan: Box::new(Statement::JoinRows(Box::new(JoinRows {
                    tree,
                    def: def.clone(),
                    where_spans_equality,
                    where_span_aliases,
                    where_filters,
                    where_equalities,
                    where_modelled,
                    needed,
                }))),
                def,
            },
        ));
        name
    });
    out.from_clause = vec![pg_query::protobuf::Node {
        node: Some(N::RangeVar(pg_query::protobuf::RangeVar {
            relname: name,
            inh: true,
            relpersistence: "p".into(),
            ..Default::default()
        })),
    }];
    Ok(out)
}

/// Register an already-planned source under a placeholder FROM name, for a
/// rewrite that has to hand the planner a source it cannot express as SQL.
pub(crate) fn register_source(src: SubSource) -> String {
    PLANNED_JOINS.with(|j| {
        let mut j = j.borrow_mut();
        let name = format!("{SEP}source{SEP}{}", j.len());
        j.push((name.clone(), src));
        name
    })
}

/// A FROM item naming a registered source.
pub(crate) fn placeholder_from(name: String) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::RangeVar(pg_query::protobuf::RangeVar {
            relname: name,
            inh: true,
            relpersistence: "p".into(),
            ..Default::default()
        })),
    }
}

/// How many join sources are planned so far (see [`truncate_planned_joins`]).
pub(crate) fn planned_joins_len() -> usize {
    PLANNED_JOINS.with(|j| j.borrow().len())
}

/// Forget the join sources planned after the first `n`: a trial plan's.
pub(crate) fn truncate_planned_joins(n: usize) {
    PLANNED_JOINS.with(|j| j.borrow_mut().truncate(n));
}

/// Forget the join sources of the statement just planned.
pub(crate) fn clear_planned_joins() {
    PLANNED_JOINS.with(|j| j.borrow_mut().clear());
}

/// A message naming a joined column by its key reads `j.id`, as PostgreSQL
/// would print it, rather than carrying the separator to the client.
pub(crate) fn unmangle(e: Error) -> Error {
    let text = e.to_string();
    if !text.contains(SEP) {
        return e;
    }
    Error::Sqlstate(e.sqlstate(), text.replace(SEP, "."))
}

/// Every joined-row key `node`'s rows carry.
pub fn node_keys(node: &JoinNode) -> Vec<String> {
    keys_of(node)
}

fn keys_of(node: &JoinNode) -> Vec<String> {
    match node {
        JoinNode::Leaf { columns, .. } => columns.iter().map(|(k, _)| k.clone()).collect(),
        JoinNode::Lateral { columns, .. } => columns.iter().map(|(k, _)| k.clone()).collect(),
        JoinNode::Join {
            left_keys,
            right_keys,
            merged,
            ..
        } => merged
            .iter()
            .map(|(k, _, _)| k.clone())
            .chain(left_keys.iter().cloned())
            .chain(right_keys.iter().cloned())
            .collect(),
    }
}

/// Plan one FROM item: a leaf, or a JOIN of two.
fn build(
    item: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    aliases: &mut Vec<String>,
    left: Option<&Scope>,
) -> Result<(JoinNode, Scope)> {
    if let Some(left) = left {
        if let Some(lateral) = lateral_leaf(item, left, lookup, params, aliases)? {
            return Ok(lateral);
        }
    }
    match item.node.as_ref() {
        Some(N::JoinExpr(j)) => build_join(j, lookup, params, aliases, left),
        Some(N::RangeVar(r)) => {
            let alias = r
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| r.relname.clone());
            let table = relation_name(r);
            if lookup(&table).is_none() {
                return Err(Error::UndefinedTable(table));
            }
            let mut rv = r.clone();
            rv.alias = None;
            let colnames = r
                .alias
                .as_ref()
                .map(|a| a.colnames.clone())
                .unwrap_or_default();
            // Only a leaf whose columns keep their names takes a filter:
            // the pushed conjunct names the table's own columns.
            let filter = if colnames.is_empty() {
                pushed_filter(&alias)
            } else {
                None
            };
            leaf_filtered(
                pg_query::protobuf::Node {
                    node: Some(N::RangeVar(rv)),
                },
                &alias,
                &colnames,
                lookup,
                params,
                aliases,
                filter,
            )
        }
        Some(N::RangeSubselect(rs)) => {
            // LATERAL with nothing to its left is an ordinary subquery.
            let mut rs = rs.clone();
            rs.lateral = false;
            let src = plan_from_subquery(&rs, lookup, params)?;
            leaf_from_source(src.plan, src.def, &src.alias, lookup, aliases)
        }
        Some(N::RangeFunction(rf)) => {
            // A function call's own alias names the source; without one,
            // PostgreSQL names it after the function.
            let alias = rf
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .or_else(|| range_function_name(rf))
                .ok_or_else(|| Error::Unsupported("this FROM function".into()))?;
            let mut rf = rf.clone();
            rf.lateral = false;
            let colnames = function_colnames(&rf);
            let bare =
                colnames.is_empty() && rf.alias.as_ref().is_some_and(|a| !a.aliasname.is_empty());
            rf.alias = keep_column_names(&rf, &alias);
            let prev = BARE_FUNCTION_ALIAS.with(|b| b.replace(bare));
            let built = leaf(
                pg_query::protobuf::Node {
                    node: Some(N::RangeFunction(rf)),
                },
                &alias,
                &colnames,
                lookup,
                params,
                aliases,
            );
            BARE_FUNCTION_ALIAS.with(|b| b.set(prev));
            built
        }
        _ => Err(Error::Unsupported("this JOIN side".into())),
    }
}

/// The alias a function keeps when planned as `SELECT * FROM f(...)`: its
/// column names, if it gave any. Without them a `ROWS FROM` of two functions
/// of one name (`unnest`, `unnest`) would answer `*` with that name twice,
/// and the second column would overwrite the first.
fn keep_column_names(
    rf: &pg_query::protobuf::RangeFunction,
    alias: &str,
) -> Option<pg_query::protobuf::Alias> {
    rf.alias
        .as_ref()
        .filter(|a| !a.colnames.is_empty())
        .map(|a| pg_query::protobuf::Alias {
            aliasname: alias.to_string(),
            colnames: a.colnames.clone(),
        })
}

/// A function's output column names from its alias: `AS t(a, b)` names them.
/// A bare `AS x` names the column only of a function returning ONE -- the
/// rule for a scalar function in FROM -- which is applied once the width is
/// known (`scalar_alias`); naming a WIDER function's first column `x` hid
/// `jsonb_each`'s `key` in a join.
fn function_colnames(rf: &pg_query::protobuf::RangeFunction) -> Vec<pg_query::protobuf::Node> {
    match rf.alias.as_ref() {
        Some(a) if !a.colnames.is_empty() => a.colnames.clone(),
        _ => Vec::new(),
    }
}

/// The names a function's columns take: the alias list, else -- for a
/// one-column function -- the bare alias.
fn scalar_alias(names: Vec<String>, function: bool, alias: &str, width: usize) -> Vec<String> {
    if names.is_empty() && function && width == 1 {
        vec![alias.to_string()]
    } else {
        names
    }
}

/// A FROM item that reads the row to its LEFT -- a `LATERAL` subquery, or a
/// function whose arguments name a column there (a function in FROM is
/// implicitly lateral, as in PostgreSQL) -- planned as SQL re-run per left
/// row. `None` for an item that reads nothing on the left, which is then an
/// ordinary leaf.
fn lateral_leaf(
    item: &pg_query::protobuf::Node,
    left: &Scope,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    aliases: &mut Vec<String>,
) -> Result<Option<(JoinNode, Scope)>> {
    let (mut inner, alias, colnames, function) = match item.node.as_ref() {
        Some(N::RangeSubselect(rs)) if rs.lateral => {
            let Some(N::SelectStmt(sel)) = rs.subquery.as_deref().and_then(|q| q.node.as_ref())
            else {
                return Err(Error::Unsupported("this LATERAL subquery".into()));
            };
            let alias = rs
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .unwrap_or_default();
            if alias.is_empty() {
                return Err(Error::Parse("subquery in FROM must have an alias".into()));
            }
            let colnames = rs
                .alias
                .as_ref()
                .map(|a| a.colnames.clone())
                .unwrap_or_default();
            ((**sel).clone(), alias, colnames, false)
        }
        Some(N::RangeFunction(rf)) => {
            let alias = rf
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .or_else(|| range_function_name(rf))
                .ok_or_else(|| Error::Unsupported("this FROM function".into()))?;
            let colnames = function_colnames(rf);
            let mut bare = rf.clone();
            bare.lateral = false;
            bare.alias = keep_column_names(rf, &alias);
            let select = pg_query::protobuf::SelectStmt {
                target_list: vec![star_target()],
                from_clause: vec![pg_query::protobuf::Node {
                    node: Some(N::RangeFunction(bare)),
                }],
                limit_option: pg_query::protobuf::LimitOption::Default as i32,
                op: pg_query::protobuf::SetOperation::SetopNone as i32,
                ..Default::default()
            };
            (select, alias, colnames, true)
        }
        _ => return Ok(None),
    };
    // The references to the left side, each bound to a parameter.
    let mut keys: Vec<String> = Vec::new();
    let mut types: Vec<String> = Vec::new();
    let base = params.len() + 1;
    let mut failure: Option<Error> = None;
    // A bare FROM-function argument the left side does not have either.
    let mut unresolved: Option<String> = None;
    let from_args = if function {
        Vec::new()
    } else {
        crate::correlated::left_function_args(&inner, lookup)
    };
    crate::correlated::walk_select(&mut inner, &mut |n| {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return Ok(());
        };
        let Some(parts) = names_of(c) else {
            return Ok(());
        };
        let points_left = match parts.as_slice() {
            [_, qualifier, _] | [qualifier, _] => left.aliases.contains(qualifier),
            // A function has no columns of its own, so a bare name in its
            // arguments can only be the left side's.
            [name] => function || from_args.contains(name),
            _ => false,
        };
        if !points_left {
            return Ok(());
        }
        let key = match left.resolve(&parts) {
            Ok(Some(k)) => k,
            Ok(None) => {
                if let [name] = parts.as_slice() {
                    unresolved.get_or_insert_with(|| name.clone());
                }
                return Ok(());
            }
            Err(e) => {
                failure.get_or_insert(e);
                return Ok(());
            }
        };
        let i = match keys.iter().position(|k| *k == key) {
            Some(i) => i,
            None => {
                let ty = left
                    .cols
                    .iter()
                    .find(|c| c.key == key)
                    .map(|c| c.pg_type.clone())
                    .unwrap_or_else(|| "text".into());
                keys.push(key);
                types.push(ty);
                keys.len() - 1
            }
        };
        // Bound as `$N::<the column's type>`, so an operator that depends on
        // the type (`j -> 'a'` on a jsonb) resolves the same with a value as
        // it would over the column.
        let param = pg_query::protobuf::Node {
            node: Some(N::ParamRef(pg_query::protobuf::ParamRef {
                number: i32::try_from(base + i).unwrap_or(i32::MAX),
                location: c.location,
            })),
        };
        n.node = Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(param)),
            type_name: Some(type_name_node(&types[i])),
            location: -1,
        })));
        Ok(())
    })?;
    if let Some(e) = failure {
        return Err(e);
    }
    if keys.is_empty() {
        // A bare FROM-function argument that is no column anywhere is
        // PostgreSQL's 42703 (the planner could only call it unsupported).
        if let Some(name) = unresolved {
            return Err(Error::UndefinedColumn(name));
        }
        // It reads nothing on its left after all.
        return Ok(None);
    }
    // Planned once over typed sample values, for the output's shape; each left
    // row re-plans it with its own.
    let mut sample_params = params.to_vec();
    sample_params.extend(types.iter().map(|t| sample_value_for_type(t)));
    let plan = match plan_select(&inner, lookup, &sample_params) {
        // It names a column nothing has: PostgreSQL's 42703, where the
        // planner could only say it cannot read the reference.
        Err(Error::Unsupported(_)) if unresolved.is_some() => {
            return Err(Error::UndefinedColumn(unresolved.unwrap_or_default()));
        }
        p => p?,
    };
    let def = sub_plan_def(&plan, lookup)?;
    let names: Vec<String> = colnames.iter().filter_map(alias_colname).collect();
    let bare_alias = matches!(
        item.node.as_ref(),
        Some(N::RangeFunction(rf)) if rf.alias.as_ref().is_some_and(|a| !a.aliasname.is_empty())
    );
    let names = scalar_alias(names, function && bare_alias, &alias, def.columns.len());
    if names.len() > def.columns.len() {
        return Err(Error::Parse(format!(
            "table \"{alias}\" has {} columns available but {} columns specified",
            def.columns.len(),
            names.len()
        )));
    }
    if aliases.contains(&alias) {
        return Err(Error::Sqlstate(
            "42712",
            format!("table name \"{alias}\" specified more than once"),
        ));
    }
    aliases.push(alias.clone());
    let sql = pg_query::protobuf::Node {
        node: Some(N::SelectStmt(Box::new(inner))),
    }
    .deparse()
    .map_err(|e| Error::Parse(e.to_string()))?;
    let mut scope = Scope {
        cols: Vec::new(),
        aliases: vec![alias.clone()],
    };
    let mut columns = Vec::new();
    for (i, c) in def.columns.iter().enumerate() {
        let name = names.get(i).cloned().unwrap_or_else(|| c.name.clone());
        let key = join_key(&alias, &name);
        columns.push((key.clone(), i));
        scope.cols.push(ScopeCol {
            alias: alias.clone(),
            name,
            key,
            pg_type: c.pg_type.clone(),
            bare: true,
            star: true,
            source: c.source,
        });
    }
    Ok(Some((
        JoinNode::Lateral {
            sql,
            params: params.to_vec(),
            keys,
            columns,
        },
        scope,
    )))
}

fn star_target() -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
            val: Some(Box::new(pg_query::protobuf::Node {
                node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                    fields: vec![pg_query::protobuf::Node {
                        node: Some(N::AStar(pg_query::protobuf::AStar {})),
                    }],
                    location: -1,
                })),
            })),
            ..Default::default()
        }))),
    }
}

pub(crate) fn range_function_name(rf: &pg_query::protobuf::RangeFunction) -> Option<String> {
    let first = rf.functions.first()?;
    let N::List(l) = first.node.as_ref()? else {
        return None;
    };
    let N::FuncCall(f) = l.items.first()?.node.as_ref()? else {
        return None;
    };
    func_name(f)
}

/// A leaf planned as `SELECT * FROM <item>`, so its rows are decoded by the
/// same path every other query's are.
fn leaf(
    item: pg_query::protobuf::Node,
    alias: &str,
    colnames: &[pg_query::protobuf::Node],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    aliases: &mut Vec<String>,
) -> Result<(JoinNode, Scope)> {
    leaf_filtered(item, alias, colnames, lookup, params, aliases, None)
}

/// `leaf`, with a WHERE the query's own WHERE implies for this leaf's rows.
fn leaf_filtered(
    item: pg_query::protobuf::Node,
    alias: &str,
    colnames: &[pg_query::protobuf::Node],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    aliases: &mut Vec<String>,
    filter: Option<pg_query::protobuf::Node>,
) -> Result<(JoinNode, Scope)> {
    // Taken before anything nested plans, which may build leaves of its own.
    let bare_alias = BARE_FUNCTION_ALIAS.with(|b| b.replace(false));
    let select = pg_query::protobuf::SelectStmt {
        where_clause: filter.map(Box::new),
        target_list: vec![pg_query::protobuf::Node {
            node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                val: Some(Box::new(pg_query::protobuf::Node {
                    node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                        fields: vec![pg_query::protobuf::Node {
                            node: Some(N::AStar(pg_query::protobuf::AStar {})),
                        }],
                        location: -1,
                    })),
                })),
                ..Default::default()
            }))),
        }],
        from_clause: vec![item],
        limit_option: pg_query::protobuf::LimitOption::Default as i32,
        op: pg_query::protobuf::SetOperation::SetopNone as i32,
        ..Default::default()
    };
    let plan = plan_select(&select, lookup, params)?;
    let mut def = sub_plan_def(&plan, lookup)?;
    let names: Vec<String> = colnames.iter().filter_map(alias_colname).collect();
    let names = scalar_alias(names, bare_alias, alias, def.columns.len());
    if names.len() > def.columns.len() {
        return Err(Error::Parse(format!(
            "table \"{alias}\" has {} columns available but {} columns specified",
            def.columns.len(),
            names.len()
        )));
    }
    // A rename changes the name the query uses, not the field the rows carry.
    let renamed: Vec<String> = def
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| names.get(i).cloned().unwrap_or_else(|| c.name.clone()))
        .collect();
    let fields: Vec<String> = def.columns.iter().map(|c| c.field()).collect();
    def.name = alias.to_string();
    let mut node_def = def.clone();
    for (c, n) in node_def.columns.iter_mut().zip(&renamed) {
        c.name = n.clone();
    }
    leaf_with_names(plan, def, alias, &renamed, &fields, &node_def, aliases)
}

fn leaf_from_source(
    plan: Box<Statement>,
    def: TableDef,
    alias: &str,
    _lookup: &dyn Fn(&str) -> Option<TableDef>,
    aliases: &mut Vec<String>,
) -> Result<(JoinNode, Scope)> {
    // A FROM-subquery's def already carries its `s(a, b)` renames, and its
    // rows are materialised POSITIONALLY into those same fields.
    let names: Vec<String> = def.columns.iter().map(|c| c.name.clone()).collect();
    let fields: Vec<String> = def.columns.iter().map(|c| c.field()).collect();
    let node_def = def.clone();
    leaf_with_names(*plan, def, alias, &names, &fields, &node_def, aliases)
}

fn leaf_with_names(
    plan: impl Into<Box<Statement>>,
    def: TableDef,
    alias: &str,
    names: &[String],
    fields: &[String],
    typed: &TableDef,
    aliases: &mut Vec<String>,
) -> Result<(JoinNode, Scope)> {
    if aliases.iter().any(|a| a == alias) {
        return Err(Error::Sqlstate(
            "42712",
            format!("table name \"{alias}\" specified more than once"),
        ));
    }
    aliases.push(alias.to_string());
    let mut scope = Scope {
        cols: Vec::new(),
        aliases: vec![alias.to_string()],
    };
    let mut columns = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let key = join_key(alias, name);
        columns.push((key.clone(), fields[i].clone()));
        scope.cols.push(ScopeCol {
            alias: alias.to_string(),
            name: name.clone(),
            key,
            pg_type: typed.columns[i].pg_type.clone(),
            bare: true,
            star: true,
            source: typed.columns[i].source,
        });
    }
    Ok((
        JoinNode::Leaf {
            plan: plan.into(),
            def,
            columns,
        },
        scope,
    ))
}

fn build_join(
    j: &pg_query::protobuf::JoinExpr,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
    aliases: &mut Vec<String>,
    outer_left: Option<&Scope>,
) -> Result<(JoinNode, Scope)> {
    use pg_query::protobuf::JoinType;
    if j.alias.is_some() {
        return Err(Error::Unsupported("an aliased JOIN".into()));
    }
    let kind = match JoinType::try_from(j.jointype) {
        Ok(JoinType::JoinInner) => JoinKind::Inner,
        Ok(JoinType::JoinLeft) => JoinKind::Left,
        Ok(JoinType::JoinRight) => JoinKind::Right,
        Ok(JoinType::JoinFull) => JoinKind::Full,
        _ => return Err(Error::Unsupported("this JOIN kind".into())),
    };
    let larg = j
        .larg
        .as_deref()
        .ok_or_else(|| Error::Parse("JOIN without a left side".into()))?;
    let rarg = j
        .rarg
        .as_deref()
        .ok_or_else(|| Error::Parse("JOIN without a right side".into()))?;
    // `reduce_outer_joins` passes down what this join's ON proves non-NULL
    // (a qualified column a strict conjunct reads): an inner join's to both
    // its sides, a LEFT join's to its nullable side and a RIGHT join's to
    // its own, a FULL join's to neither. A NULL-extended row there never
    // passes the ON, so an outer join below with that table on its
    // nullable side is reduced too.
    // The kind this join is reduced to by what is proven above it, which
    // decides what it passes down (as `reduce_outer_joins_pass2` decides
    // before it recurses).
    let saved = NONNULLABLE.with(|n| n.borrow().clone());
    let proven = |side: &pg_query::protobuf::Node| {
        let mut names = Vec::new();
        subtree_aliases(side, &mut names);
        names.iter().any(|a| saved.contains(a))
    };
    let reduced = match kind {
        JoinKind::Left if proven(rarg) => JoinKind::Inner,
        JoinKind::Right if proven(larg) => JoinKind::Inner,
        JoinKind::Full => match (proven(larg), proven(rarg)) {
            (true, true) => JoinKind::Inner,
            (true, false) => JoinKind::Left,
            (false, true) => JoinKind::Right,
            (false, false) => JoinKind::Full,
        },
        k => k,
    };
    let local: Vec<String> = match (&reduced, j.quals.as_deref()) {
        (JoinKind::Full, _) | (_, None) => Vec::new(),
        (_, Some(q)) => {
            let mut conjuncts = Vec::new();
            split_and(q, &mut conjuncts);
            conjuncts
                .iter()
                .flat_map(|c| strict_aliases(c, &|_| None))
                .collect()
        }
    };
    let with_local = |on_side: bool| {
        NONNULLABLE.with(|n| {
            let mut n = n.borrow_mut();
            *n = saved.clone();
            if on_side {
                n.extend(local.iter().cloned());
            }
        })
    };
    with_local(matches!(reduced, JoinKind::Inner | JoinKind::Right));
    let left = build(larg, lookup, params, aliases, outer_left);
    let (left, lscope) = match left {
        Ok(l) => l,
        Err(e) => {
            NONNULLABLE.with(|n| *n.borrow_mut() = saved);
            return Err(e);
        }
    };
    // The right side may be LATERAL over everything to its left.
    let mut visible = outer_left.cloned().unwrap_or_default();
    visible.cols.extend(lscope.cols.iter().cloned());
    visible.aliases.extend(lscope.aliases.iter().cloned());
    with_local(matches!(reduced, JoinKind::Inner | JoinKind::Left));
    let right = build(rarg, lookup, params, aliases, Some(&visible));
    NONNULLABLE.with(|n| *n.borrow_mut() = saved);
    let (right, rscope) = right?;
    // `reduce_outer_joins`: a WHERE that cannot hold for a NULL-extended row
    // makes a LEFT join an inner one, which PostgreSQL then plans as such.
    // A RIGHT join likewise, by its left side; a FULL join is a LEFT join
    // when only its left side cannot be NULL-extended, a RIGHT one when
    // only its right, and inner when both.
    let nonnullable = |scope: &Scope| {
        NONNULLABLE.with(|n| {
            let n = n.borrow();
            scope.cols.iter().any(|c| n.contains(&c.alias))
        })
    };
    let kind = match kind {
        JoinKind::Left if nonnullable(&rscope) => JoinKind::Inner,
        JoinKind::Right if nonnullable(&lscope) => JoinKind::Inner,
        JoinKind::Full => match (nonnullable(&lscope), nonnullable(&rscope)) {
            (true, true) => JoinKind::Inner,
            (true, false) => JoinKind::Left,
            (false, true) => JoinKind::Right,
            (false, false) => JoinKind::Full,
        },
        kind => kind,
    };
    if matches!(right, JoinNode::Lateral { .. }) && matches!(kind, JoinKind::Right | JoinKind::Full)
    {
        return Err(Error::Sqlstate(
            "42P10",
            "The combining JOIN type must be INNER or LEFT for a LATERAL reference.".into(),
        ));
    }
    let left_keys = keys_of(&left);
    let right_keys = keys_of(&right);

    // USING (a, b) / NATURAL: each named column once, merged from both sides.
    let using: Vec<String> = if j.is_natural {
        lscope
            .cols
            .iter()
            .filter(|c| c.bare && c.star)
            .filter(|c| {
                rscope
                    .cols
                    .iter()
                    .any(|r| r.bare && r.star && r.name == c.name)
            })
            .map(|c| c.name.clone())
            .collect()
    } else {
        j.using_clause
            .iter()
            .filter_map(|n| match n.node.as_ref()? {
                N::String(s) => Some(s.sval.clone()),
                _ => None,
            })
            .collect()
    };
    let mut merged = Vec::new();
    let mut merged_cols = Vec::new();
    let mut hidden: Vec<String> = Vec::new();
    for name in &using {
        let side = |scope: &Scope, which: &str| -> Result<ScopeCol> {
            let hits: Vec<&ScopeCol> = scope
                .cols
                .iter()
                .filter(|c| c.bare && &c.name == name)
                .collect();
            match hits.as_slice() {
                [one] => Ok((*one).clone()),
                [] => Err(Error::Sqlstate(
                    "42703",
                    format!("column \"{name}\" specified in USING clause does not exist in {which} table"),
                )),
                _ => Err(Error::Sqlstate(
                    "42702",
                    format!("common column name \"{name}\" appears more than once in {which} table"),
                )),
            }
        };
        let l = side(&lscope, "left")?;
        let r = side(&rscope, "right")?;
        let key = join_key(&format!("{SEP}{}", merged.len()), name);
        merged.push((key.clone(), l.key.clone(), r.key.clone()));
        hidden.push(l.key.clone());
        hidden.push(r.key.clone());
        // The merged column is the left side's (the right's under a RIGHT
        // join); a FULL join's is a COALESCE of both, read from neither.
        let source = match pg_query::protobuf::JoinType::try_from(j.jointype) {
            Ok(pg_query::protobuf::JoinType::JoinFull) => None,
            Ok(pg_query::protobuf::JoinType::JoinRight) => r.source,
            _ => l.source,
        };
        merged_cols.push(ScopeCol {
            alias: String::new(),
            name: name.clone(),
            key,
            pg_type: l.pg_type.clone(),
            bare: true,
            star: true,
            source,
        });
    }
    let mut scope = Scope {
        cols: merged_cols,
        aliases: lscope
            .aliases
            .iter()
            .chain(&rscope.aliases)
            .cloned()
            .collect(),
    };
    for c in lscope.cols.iter().chain(&rscope.cols) {
        let mut c = c.clone();
        if hidden.contains(&c.key) {
            c.bare = false;
            c.star = false;
        }
        scope.cols.push(c);
    }

    // The ON clause, over this join's own two sides; USING is its column
    // equalities. A merged column is not visible inside ON, as in PostgreSQL.
    let mut equi: Vec<(String, String)> = merged
        .iter()
        .map(|(_, l, r)| (l.clone(), r.clone()))
        .collect();
    let mut equi_reversed = vec![false; equi.len()];
    let mut expr_keys = Vec::new();
    let mut on_aliases = Vec::new();
    let mut on_other = false;
    let on = match j.quals.as_deref() {
        None if merged.is_empty() => None,
        None => {
            let parts: Vec<String> = merged
                .iter()
                .map(|(_, l, r)| format!("\"{l}\" = \"{r}\""))
                .collect();
            let N::SelectStmt(sel) = parse_one(&format!("SELECT {}", parts.join(" AND ")))? else {
                return Err(Error::Internal("USING did not parse".into()));
            };
            let expr = sel
                .target_list
                .first()
                .and_then(|t| match t.node.as_ref() {
                    Some(N::ResTarget(r)) => r.val.as_deref().cloned(),
                    _ => None,
                })
                .ok_or_else(|| Error::Internal("USING did not parse".into()))?;
            Some(on_expr(&expr, &scope, params)?)
        }
        Some(quals) => {
            let mut q = quals.clone();
            let side_scope = Scope {
                cols: scope
                    .cols
                    .iter()
                    .filter(|c| !c.key.starts_with(SEP))
                    .cloned()
                    .collect(),
                aliases: scope.aliases.clone(),
            };
            rewrite_expr(&mut q, &side_scope, &[])?;
            on_aliases = key_aliases(&q);
            for (l, r, reversed) in equalities(&q, &left_keys, &right_keys) {
                equi.push((l, r));
                equi_reversed.push(reversed);
            }
            expr_keys = expr_equalities(&q, &left_keys, &right_keys)
                .into_iter()
                .map(|(l, r)| {
                    Ok(ExprEquality {
                        left: on_expr(l, &side_scope, params)?,
                        right: on_expr(r, &side_scope, params)?,
                        left_ops: expr_ops(l),
                        right_ops: expr_ops(r),
                        left_column: bare_key(l),
                        right_column: bare_key(r),
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            let conjuncts = match q.node.as_ref() {
                Some(N::BoolExpr(b))
                    if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 =>
                {
                    b.args.len()
                }
                _ => 1,
            };
            on_other = conjuncts > equi.len() - merged.len() + expr_keys.len();
            Some(on_expr(&q, &side_scope, params)?)
        }
    };
    // `reduce_outer_joins` flips a RIGHT join into the LEFT join of its
    // sides swapped, which is what PostgreSQL then plans: every planner
    // routine after it sees only LEFT. The columns keep their written order
    // (the scope); a USING column still takes the preserved side's value.
    let (kind, left, right, left_keys, right_keys) = if kind == JoinKind::Right {
        equi = equi.into_iter().map(|(l, r)| (r, l)).collect();
        equi_reversed = equi_reversed.into_iter().map(|r| !r).collect();
        merged = merged.into_iter().map(|(k, l, r)| (k, r, l)).collect();
        expr_keys = expr_keys
            .into_iter()
            .map(|e| ExprEquality {
                left: e.right,
                right: e.left,
                left_ops: e.right_ops,
                right_ops: e.left_ops,
                left_column: e.right_column,
                right_column: e.left_column,
            })
            .collect();
        (JoinKind::Left, right, left, right_keys, left_keys)
    } else {
        (kind, left, right, left_keys, right_keys)
    };
    Ok((
        JoinNode::Join {
            kind,
            left: Box::new(left),
            right: Box::new(right),
            on,
            ec: (kind == JoinKind::Inner && merged.is_empty())
                .then(|| {
                    EC_PLAN.with(|p| {
                        p.borrow()
                            .as_ref()
                            .and_then(|plan| plan.for_join(&left_keys, &right_keys))
                    })
                })
                .flatten(),
            equi,
            equi_reversed,
            merged,
            expr_keys,
            on_aliases,
            on_other,
            left_keys,
            right_keys,
        },
        scope,
    ))
}

/// The table aliases of the joined-row keys `n` reads.
fn key_aliases(n: &pg_query::protobuf::Node) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut n = n.clone();
    let _ = walk_expr(&mut n, &mut |x| {
        if let Some(N::ColumnRef(c)) = x.node.as_ref() {
            if let Some(alias) = column_ref_name(c)
                .as_deref()
                .and_then(|k| k.split_once(SEP))
                .map(|(a, _)| a.to_string())
            {
                if !out.contains(&alias) {
                    out.push(alias);
                }
            }
        }
        Ok(())
    });
    out
}

/// A WHERE conjunct over two or more tables, as a join filter.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinFilter {
    /// The tables it reads.
    pub aliases: Vec<String>,
    /// It, over the joined keys.
    pub expr: ColumnExpr,
    pub kind: FilterKind,
    /// The operators it evaluates a row (`cost_qual_eval`).
    pub ops: usize,
}

/// What a join filter's selectivity is estimated as with no statistics.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterKind {
    /// `<`, `<=`, `>`, `>=` between two columns: `DEFAULT_INEQ_SEL`
    /// (`scalarineqsel` with no constant to compare against).
    Inequality,
    /// `<>` or `IS DISTINCT FROM` between two columns (joined keys):
    /// `neqjoinsel`, one minus `eqjoinsel`'s `1 / max(nd1, nd2)`
    /// (`clause_selectivity` takes a DistinctExpr as its `=`, negated).
    NotEqual(String, String),
    /// A column equal to a constant: `var_eq_const` with no statistics,
    /// `1 / nd`.
    EqualsConst(String),
    /// An OR of the above: `s1 + s2 - s1 * s2`, arm by arm
    /// (`clauselist_selectivity_or`).
    Or(Vec<FilterKind>),
    /// Anything else, which is not estimated here.
    Other,
}

/// The operators `n` evaluates: each operator and IS DISTINCT FROM.
fn operator_count(n: &pg_query::protobuf::Node) -> usize {
    let mut count = 0;
    let mut node = n.clone();
    let _ = walk_expr(&mut node, &mut |x| {
        if let Some(N::AExpr(e)) = x.node.as_ref() {
            if e.kind == pg_query::protobuf::AExprKind::AexprOp as i32
                || e.kind == pg_query::protobuf::AExprKind::AexprDistinct as i32
            {
                count += 1;
            }
        }
        Ok(())
    });
    count
}

/// How `n`, a WHERE conjunct over joined keys, is estimated.
fn filter_kind(n: &pg_query::protobuf::Node) -> FilterKind {
    use pg_query::protobuf::{AExprKind, BoolExprType};
    let key = |x: Option<&pg_query::protobuf::Node>| match x?.node.as_ref()? {
        N::ColumnRef(c) if c.fields.len() == 1 => column_ref_name(c),
        _ => None,
    };
    let constant = |x: Option<&pg_query::protobuf::Node>| {
        matches!(x.and_then(|n| n.node.as_ref()), Some(N::AConst(_)))
    };
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::OrExpr as i32 => {
            let arms: Vec<FilterKind> = b.args.iter().map(filter_kind).collect();
            if arms.contains(&FilterKind::Other) {
                FilterKind::Other
            } else {
                FilterKind::Or(arms)
            }
        }
        Some(N::AExpr(e)) if e.kind == AExprKind::AexprDistinct as i32 => {
            match (key(e.lexpr.as_deref()), key(e.rexpr.as_deref())) {
                (Some(l), Some(r)) => FilterKind::NotEqual(l, r),
                _ => FilterKind::Other,
            }
        }
        Some(N::AExpr(e)) if e.kind == AExprKind::AexprOp as i32 => {
            let (l, r) = (e.lexpr.as_deref(), e.rexpr.as_deref());
            match operator_name(e) {
                // No statistics to compare by, an expression's or a
                // column's: `DEFAULT_INEQ_SEL`.
                Ok("<" | "<=" | ">" | ">=") => FilterKind::Inequality,
                Ok("<>") => match (key(l), key(r)) {
                    (Some(a), Some(b)) => FilterKind::NotEqual(a, b),
                    _ => FilterKind::Other,
                },
                Ok("=") => match (key(l), key(r)) {
                    (Some(a), None) if constant(r) => FilterKind::EqualsConst(a),
                    (None, Some(b)) if constant(l) => FilterKind::EqualsConst(b),
                    _ => FilterKind::Other,
                },
                _ => FilterKind::Other,
            }
        }
        _ => FilterKind::Other,
    }
}

/// The ON predicate as a row expression over the joined keys.
fn on_expr(node: &pg_query::protobuf::Node, scope: &Scope, params: &[Bson]) -> Result<ColumnExpr> {
    let fields = scope.fields();
    let mut sample = Document::new();
    for c in &scope.cols {
        sample.insert(c.key.clone(), sample_value_for_type(&c.pg_type));
    }
    row_column_expr(node, &fields, params, &sample)
}

/// `left.x = right.y` conjuncts of an ON, as `(left key, right key,
/// written right key first)`.
fn equalities(
    node: &pg_query::protobuf::Node,
    left: &[String],
    right: &[String],
) -> Vec<(String, String, bool)> {
    use pg_query::protobuf::BoolExprType;
    match node.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => b
            .args
            .iter()
            .flat_map(|a| equalities(a, left, right))
            .collect(),
        Some(N::AExpr(e))
            if e.kind == pg_query::protobuf::AExprKind::AexprOp as i32
                && operator_name(e) == Ok("=") =>
        {
            let key = |n: Option<&pg_query::protobuf::Node>| -> Option<String> {
                match n?.node.as_ref()? {
                    N::ColumnRef(c) if c.fields.len() == 1 => column_ref_name(c),
                    _ => None,
                }
            };
            match (key(e.lexpr.as_deref()), key(e.rexpr.as_deref())) {
                (Some(a), Some(b)) if left.contains(&a) && right.contains(&b) => {
                    vec![(a, b, false)]
                }
                (Some(a), Some(b)) if left.contains(&b) && right.contains(&a) => vec![(b, a, true)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// `left expr = right expr` conjuncts of an ON that `equalities` does not
/// take (one side or both an expression), each operand reading only one
/// side's keys, as `(left operand, right operand)`. Only operators, casts,
/// constants and columns: anything that could be volatile is left to `on`.
fn expr_equalities<'a>(
    node: &'a pg_query::protobuf::Node,
    left: &[String],
    right: &[String],
) -> Vec<(&'a pg_query::protobuf::Node, &'a pg_query::protobuf::Node)> {
    use pg_query::protobuf::BoolExprType;
    fn refs(n: &pg_query::protobuf::Node, out: &mut Vec<String>) -> bool {
        match n.node.as_ref() {
            Some(N::ColumnRef(c)) if c.fields.len() == 1 => match column_ref_name(c) {
                Some(k) => {
                    out.push(k);
                    true
                }
                None => false,
            },
            Some(N::AConst(_)) | Some(N::ParamRef(_)) => true,
            Some(N::TypeCast(t)) => t.arg.as_deref().is_some_and(|a| refs(a, out)),
            Some(N::AExpr(e)) if e.kind == pg_query::protobuf::AExprKind::AexprOp as i32 => {
                e.lexpr.as_deref().is_none_or(|x| refs(x, out))
                    && e.rexpr.as_deref().is_none_or(|x| refs(x, out))
            }
            _ => false,
        }
    }
    let side = |n: &pg_query::protobuf::Node| -> Option<bool> {
        let mut keys = Vec::new();
        if !refs(n, &mut keys) || keys.is_empty() {
            return None;
        }
        if keys.iter().all(|k| left.contains(k)) {
            Some(true)
        } else if keys.iter().all(|k| right.contains(k)) {
            Some(false)
        } else {
            None
        }
    };
    let bare = |n: &pg_query::protobuf::Node| matches!(n.node.as_ref(), Some(N::ColumnRef(c)) if c.fields.len() == 1);
    match node.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == BoolExprType::AndExpr as i32 => b
            .args
            .iter()
            .flat_map(|a| expr_equalities(a, left, right))
            .collect(),
        Some(N::AExpr(e))
            if e.kind == pg_query::protobuf::AExprKind::AexprOp as i32
                && operator_name(e) == Ok("=") =>
        {
            let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
                return Vec::new();
            };
            if bare(l) && bare(r) {
                return Vec::new();
            }
            match (side(l), side(r)) {
                (Some(true), Some(false)) => vec![(l, r)],
                (Some(false), Some(true)) => vec![(r, l)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// The joined-row key `n` names, when it is a bare column.
fn bare_key(n: &pg_query::protobuf::Node) -> Option<String> {
    match n.node.as_ref() {
        Some(N::ColumnRef(c)) if c.fields.len() == 1 => column_ref_name(c),
        _ => None,
    }
}

/// The operators an expression evaluates, each a `cpu_operator_cost`.
fn expr_ops(n: &pg_query::protobuf::Node) -> u32 {
    match n.node.as_ref() {
        Some(N::TypeCast(t)) => t.arg.as_deref().map_or(0, expr_ops),
        Some(N::AExpr(e)) => {
            1 + e.lexpr.as_deref().map_or(0, expr_ops) + e.rexpr.as_deref().map_or(0, expr_ops)
        }
        _ => 0,
    }
}

fn names_of(c: &pg_query::protobuf::ColumnRef) -> Option<Vec<String>> {
    c.fields
        .iter()
        .map(|f| match f.node.as_ref()? {
            N::String(s) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

fn key_ref(key: String, location: i32) -> N {
    N::ColumnRef(pg_query::protobuf::ColumnRef {
        fields: vec![pg_query::protobuf::Node {
            node: Some(N::String(pg_query::protobuf::String { sval: key })),
        }],
        location,
    })
}

/// Rewrite every column reference under `node` to its key. `keep` holds bare
/// names left alone (output aliases an ORDER BY may name).
fn rewrite_expr(node: &mut pg_query::protobuf::Node, scope: &Scope, keep: &[String]) -> Result<()> {
    walk_expr(node, &mut |n| {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return Ok(());
        };
        let Some(parts) = names_of(c) else {
            return Ok(());
        };
        if parts.len() == 1 && keep.contains(&parts[0]) {
            return Ok(());
        }
        let resolved = scope.resolve(&parts).inspect_err(|_| {
            if c.location >= 0 {
                set_error_location(c.location);
            }
        })?;
        if let Some(key) = resolved {
            n.node = Some(key_ref(key, c.location));
        }
        Ok(())
    })
}

/// Rewrite the query's own clauses to read the join's keys, expanding `*`.
fn rewrite_query(s: &mut pg_query::protobuf::SelectStmt, scope: &Scope) -> Result<()> {
    let mut targets = Vec::new();
    for t in std::mem::take(&mut s.target_list) {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            targets.push(t);
            continue;
        };
        let star = match rt.val.as_deref().and_then(|v| v.node.as_ref()) {
            Some(N::ColumnRef(c))
                if matches!(
                    c.fields.last().and_then(|f| f.node.as_ref()),
                    Some(N::AStar(_))
                ) =>
            {
                Some(names_of(&pg_query::protobuf::ColumnRef {
                    fields: c.fields[..c.fields.len() - 1].to_vec(),
                    location: c.location,
                }))
            }
            _ => None,
        };
        if let Some(prefix) = star {
            let prefix = prefix.unwrap_or_default();
            let alias = match prefix.as_slice() {
                [] => None,
                [.., a] => Some(a.clone()),
            };
            if let Some(a) = &alias {
                if !scope.aliases.contains(a) {
                    return Err(Error::Sqlstate(
                        "42P01",
                        format!("missing FROM-clause entry for table \"{a}\""),
                    ));
                }
            }
            for c in &scope.cols {
                let take = match &alias {
                    None => c.star,
                    Some(a) => &c.alias == a,
                };
                if take {
                    targets.push(pg_query::protobuf::Node {
                        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                            name: c.name.clone(),
                            val: Some(Box::new(pg_query::protobuf::Node {
                                node: Some(key_ref(c.key.clone(), -1)),
                            })),
                            location: -1,
                            ..Default::default()
                        }))),
                    });
                }
            }
            continue;
        }
        let mut t = t;
        if let Some(N::ResTarget(rt)) = t.node.as_mut() {
            // An unnamed column target keeps the COLUMN's name as its output
            // name, which the key it is rewritten to would otherwise replace.
            if rt.name.is_empty() {
                if let Some(N::ColumnRef(c)) = rt.val.as_deref().and_then(|v| v.node.as_ref()) {
                    if let Some(last) = names_of(c).and_then(|p| p.last().cloned()) {
                        rt.name = last;
                    }
                } else if let Some(v) = rt.val.as_deref() {
                    // So does any expression named after a column inside
                    // it (`a.x::text` is `x`), which the rewrite would
                    // otherwise name after the key.
                    let name = expression_column_name(v);
                    if name != "?column?" {
                        rt.name = name;
                    }
                }
            }
            if let Some(v) = rt.val.as_deref_mut() {
                rewrite_expr(v, scope, &[])?;
            }
        }
        targets.push(t);
    }
    s.target_list = targets;
    // ORDER BY a bare name prefers an OUTPUT column, as in PostgreSQL.
    let outputs: Vec<String> = s
        .target_list
        .iter()
        .filter_map(|t| match t.node.as_ref() {
            Some(N::ResTarget(r)) if !r.name.is_empty() => Some(r.name.clone()),
            _ => None,
        })
        .collect();
    if let Some(w) = s.where_clause.as_deref_mut() {
        rewrite_expr(w, scope, &[])?;
    }
    if let Some(h) = s.having_clause.as_deref_mut() {
        rewrite_expr(h, scope, &[])?;
    }
    for g in &mut s.group_clause {
        rewrite_expr(g, scope, &[])?;
    }
    for d in &mut s.distinct_clause {
        rewrite_expr(d, scope, &outputs)?;
    }
    // Only a key that IS a bare name may name an output: inside an
    // expression (`grouping(name)`, `name || 'x'`) a name is an input
    // column, as in PostgreSQL (batch 68).
    for o in &mut s.sort_clause {
        let bare = matches!(o.node.as_ref(), Some(N::SortBy(sb))
            if matches!(sb.node.as_deref().and_then(|n| n.node.as_ref()),
                Some(N::ColumnRef(c)) if names_of(c).is_some_and(|p| p.len() == 1)));
        if !bare {
            // An output name inside an expression that no FROM item has is
            // PostgreSQL's 42703 (it reached the aggregate planner as a
            // hidden column and came back as a grouping error).
            walk_expr(o, &mut |n| {
                if let Some(N::ColumnRef(c)) = n.node.as_ref() {
                    if let Some([name]) = names_of(c).as_deref() {
                        if outputs.contains(name)
                            && scope.resolve(std::slice::from_ref(name))?.is_none()
                        {
                            if c.location >= 0 {
                                set_error_location(c.location);
                            }
                            return Err(Error::Sqlstate(
                                "42703",
                                format!("column \"{name}\" does not exist"),
                            ));
                        }
                    }
                }
                Ok(())
            })?;
        }
        rewrite_expr(o, scope, if bare { &outputs } else { &[] })?;
    }
    for w in &mut s.window_clause {
        rewrite_expr(w, scope, &[])?;
    }
    Ok(())
}

/// The set-returning functions a select list may call over a column.
pub(crate) const SELECT_LIST_SRFS: &[&str] = &[
    "unnest",
    "generate_series",
    "generate_subscripts",
    "regexp_split_to_table",
    "string_to_table",
    "jsonb_array_elements",
    "json_array_elements",
    "jsonb_array_elements_text",
    "json_array_elements_text",
    "jsonb_object_keys",
    "json_object_keys",
    "regexp_matches",
    "jsonb_path_query",
    "jsonb_path_query_tz",
    "pg_snapshot_xip",
    "txid_snapshot_xip",
];

/// The WHERE conjuncts a table leaf can apply to its own rows, by alias.
///
/// A conjunct qualifies when every column it reads belongs to ONE alias
/// (qualified, or unqualified and in only one table), and it is built only
/// of operators, casts, constants, parameters and calls of IMMUTABLE
/// built-ins (no other function call, which may be volatile and would run
/// twice, and no subquery). Its leaf must
/// be on the PRESERVED side of every outer join above it: filtering the
/// nullable side before the join would keep the NULL-extended rows
/// PostgreSQL's WHERE drops. The conjunct also stays in the outer WHERE, so
/// the answer is the same whether or not the leaf applied it -- the leaf
/// just reads fewer rows, and can reach an index. One that calls a volatile
/// or stable function, or a user function, is MOVED instead (its positions
/// among the WHERE's conjuncts are the last value): it runs once a row of
/// its table, as in PostgreSQL's scan, not again per joined row (RP-019).
#[allow(clippy::type_complexity)]
fn pushdown_filters(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> (
    Vec<(String, pg_query::protobuf::Node)>,
    Vec<String>,
    Option<EcPlan>,
    Vec<bool>,
    Vec<usize>,
) {
    let empty = pg_query::protobuf::Node::default();
    let w = s.where_clause.as_deref();
    // An unqualified column belongs to the one table in the FROM that has
    // it (`colNameToVar`); one two tables have is PostgreSQL's 42702, left
    // to the planner of the rewritten query. Known only when every item is
    // a stored table: a subquery's or a function's columns, or a CTE that
    // shadows a table's name, are not looked up here.
    let mut tables = Vec::new();
    let bare_known = s.with_clause.is_none()
        && s.from_clause
            .iter()
            .all(|item| stored_tables(item, lookup, &mut tables));
    let owner = |col: &str| -> Option<String> {
        if !bare_known {
            return None;
        }
        let mut owners = tables
            .iter()
            .filter(|(_, def): &&(String, TableDef)| def.column(col).is_some());
        let (alias, _) = owners.next()?;
        owners.next().is_none().then(|| alias.clone())
    };
    let mut conjuncts = Vec::new();
    split_and(w.unwrap_or(&empty), &mut conjuncts);
    if w.is_none() {
        conjuncts.clear();
    }
    let nonnullable: Vec<String> = conjuncts
        .iter()
        .flat_map(|c| strict_aliases(c, &owner))
        .collect();
    let mut preserved = Vec::new();
    for item in &s.from_clause {
        preserved_aliases(item, false, &nonnullable, &mut preserved);
    }
    let ec = if bare_known {
        equivalence_classes(s, &tables, &conjuncts, &nonnullable, &owner)
    } else {
        None
    };
    let mut filterable = Vec::new();
    for item in &s.from_clause {
        filtered_leaves(item, &mut filterable);
    }
    // Which conjuncts the scans or the classes take (in WHERE order), and
    // which of them are moved rather than copied.
    let mut placed = vec![false; conjuncts.len()];
    let mut moved = Vec::new();
    let mut pushed: Vec<(String, pg_query::protobuf::Node)> = Vec::new();
    for (i, c) in conjuncts.iter().enumerate() {
        if ec
            .as_ref()
            .is_some_and(|(_, absorbed, _)| absorbed.contains(&i))
        {
            placed[i] = true;
            continue;
        }
        let Some((alias, move_it)) = movable_alias(c, &owner) else {
            continue;
        };
        // Only a stored table's leaf applies a pushed filter (`build`): a
        // conjunct that is moved, not copied, goes nowhere else.
        if move_it && !filterable.contains(&alias) {
            continue;
        }
        if preserved.contains(&alias) {
            placed[i] = true;
            if move_it {
                moved.push(i);
            }
            let mut c = (*c).clone();
            strip_qualifier(&mut c);
            pushed.push((alias, c));
        }
    }
    let ec = ec.map(|(plan, _, consts)| {
        pushed.extend(consts);
        plan
    });
    (pushed, nonnullable, ec, placed, moved)
}

/// The aliases of the FROM's table leaves that take a pushed filter: a
/// table not renamed column by column (`build`'s `RangeVar` arm).
fn filtered_leaves(item: &pg_query::protobuf::Node, out: &mut Vec<String>) {
    match item.node.as_ref() {
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                filtered_leaves(side, out);
            }
        }
        Some(N::RangeVar(r)) if r.alias.as_ref().is_none_or(|a| a.colnames.is_empty()) => {
            out.push(
                r.alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| r.relname.clone()),
            );
        }
        _ => {}
    }
}

/// One side of an equality atom: a table's column, or a constant.
enum EcMember<'a> {
    Column(String, String),
    Const(&'a pg_query::protobuf::Node),
}

/// The equivalence classes of a join of two stored tables
/// (`process_equivalence`, `generate_base_implied_equalities`): the ON's
/// equalities (an inner join's, or a LEFT join's the WHERE makes inner)
/// and then the WHERE's, each `column = column` or `column = constant`,
/// merged where they share a column. Returns the plan, the WHERE
/// conjuncts the classes absorbed (their implied equalities replace
/// them), and the `column = constant` filters to push into each side.
/// `None` for any other FROM, or when a class mixes column types, holds
/// two constants or a NULL.
#[allow(clippy::type_complexity)]
fn equivalence_classes(
    s: &pg_query::protobuf::SelectStmt,
    tables: &[(String, TableDef)],
    conjuncts: &[&pg_query::protobuf::Node],
    nonnullable: &[String],
    owner: &dyn Fn(&str) -> Option<String>,
) -> Option<(EcPlan, Vec<usize>, Vec<(String, pg_query::protobuf::Node)>)> {
    use pg_query::protobuf::JoinType;
    let [(a1, _), (a2, _)] = tables else {
        return None;
    };
    if a1 == a2 {
        return None;
    }
    // The ON's conjuncts, ahead of the WHERE's (`deconstruct_jointree`).
    let mut on = Vec::new();
    match s.from_clause.as_slice() {
        [one] => {
            let Some(N::JoinExpr(j)) = one.node.as_ref() else {
                return None;
            };
            if j.is_natural || !j.using_clause.is_empty() || j.alias.is_some() {
                return None;
            }
            let inner = match JoinType::try_from(j.jointype) {
                Ok(JoinType::JoinInner) => true,
                Ok(JoinType::JoinLeft) => {
                    let mut right = Vec::new();
                    if let Some(r) = j.rarg.as_deref() {
                        table_aliases(r, &mut right);
                    }
                    right.iter().any(|a| nonnullable.contains(a))
                }
                Ok(JoinType::JoinRight) => {
                    let mut left = Vec::new();
                    if let Some(l) = j.larg.as_deref() {
                        table_aliases(l, &mut left);
                    }
                    left.iter().any(|a| nonnullable.contains(a))
                }
                _ => false,
            };
            if !inner {
                return None;
            }
            if let Some(q) = j.quals.as_deref() {
                split_and(q, &mut on);
            }
        }
        [_, _] => {}
        _ => return None,
    }
    let column = |n: &pg_query::protobuf::Node| -> Option<(String, String)> {
        let Some(N::ColumnRef(c)) = n.node.as_ref() else {
            return None;
        };
        let parts = names_of(c)?;
        match parts.as_slice() {
            [q, col] => Some((q.clone(), col.clone())),
            [col] => Some((owner(col)?, col.clone())),
            _ => None,
        }
    };
    fn member<'a>(
        n: &'a pg_query::protobuf::Node,
        column: &dyn Fn(&pg_query::protobuf::Node) -> Option<(String, String)>,
    ) -> Option<EcMember<'a>> {
        if let Some((alias, col)) = column(n) {
            return Some(EcMember::Column(alias, col));
        }
        let is_const = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
            Some(N::AConst(c)) => !c.isnull,
            Some(N::ParamRef(_)) => true,
            _ => false,
        };
        match n.node.as_ref() {
            Some(N::TypeCast(t)) if t.arg.as_deref().is_some_and(is_const) => {
                Some(EcMember::Const(n))
            }
            _ if is_const(n) => Some(EcMember::Const(n)),
            _ => None,
        }
    }
    fn atom<'a>(
        n: &'a pg_query::protobuf::Node,
        column: &dyn Fn(&pg_query::protobuf::Node) -> Option<(String, String)>,
    ) -> Option<(EcMember<'a>, EcMember<'a>)> {
        let Some(N::AExpr(e)) = n.node.as_ref() else {
            return None;
        };
        if e.kind != pg_query::protobuf::AExprKind::AexprOp as i32 || operator_name(e) != Ok("=") {
            return None;
        }
        let (l, r) = (
            member(e.lexpr.as_deref()?, column)?,
            member(e.rexpr.as_deref()?, column)?,
        );
        match (&l, &r) {
            (EcMember::Const(_), EcMember::Const(_)) => None,
            _ => Some((l, r)),
        }
    }
    let type_of = |alias: &str, col: &str| -> Option<String> {
        let (_, def) = tables.iter().find(|(a, _)| a == alias)?;
        Some(def.column(col)?.pg_type.clone())
    };
    // Classes: their column members in order, and their constant.
    struct Class<'a> {
        members: Vec<(String, String)>,
        constant: Option<&'a pg_query::protobuf::Node>,
    }
    let mut classes: Vec<Class<'_>> = Vec::new();
    let mut absorbed = Vec::new();
    let atoms = on
        .iter()
        .map(|n| (None, *n))
        .chain(conjuncts.iter().enumerate().map(|(i, n)| (Some(i), *n)));
    for (where_index, n) in atoms {
        let Some((l, r)) = atom(n, &column) else {
            continue;
        };
        let find = |classes: &Vec<Class<'_>>, m: &(String, String)| {
            classes.iter().position(|c| c.members.contains(m))
        };
        match (l, r) {
            (EcMember::Column(xa, xc), EcMember::Column(ya, yc)) => {
                let (x, y) = ((xa, xc), (ya, yc));
                if type_of(&x.0, &x.1)? != type_of(&y.0, &y.1)? {
                    return None;
                }
                match (find(&classes, &x), find(&classes, &y)) {
                    (Some(i), Some(j)) if i == j => {}
                    (Some(i), Some(j)) => {
                        let (lo, hi) = (i.min(j), i.max(j));
                        let merged = classes.remove(hi);
                        if merged.constant.is_some() && classes[lo].constant.is_some() {
                            return None;
                        }
                        classes[lo].constant = classes[lo].constant.or(merged.constant);
                        for m in merged.members {
                            if !classes[lo].members.contains(&m) {
                                classes[lo].members.push(m);
                            }
                        }
                    }
                    (Some(i), None) => classes[i].members.push(y),
                    (None, Some(j)) => classes[j].members.push(x),
                    (None, None) => classes.push(Class {
                        members: vec![x, y],
                        constant: None,
                    }),
                }
            }
            (EcMember::Column(xa, xc), EcMember::Const(k))
            | (EcMember::Const(k), EcMember::Column(xa, xc)) => {
                let x = (xa, xc);
                type_of(&x.0, &x.1)?;
                match find(&classes, &x) {
                    Some(i) if classes[i].constant.is_some() => return None,
                    Some(i) => classes[i].constant = Some(k),
                    None => classes.push(Class {
                        members: vec![x],
                        constant: Some(k),
                    }),
                }
            }
            _ => continue,
        }
        if let Some(i) = where_index {
            absorbed.push(i);
        }
    }
    if classes.is_empty() {
        return None;
    }
    if classes.iter().any(|c| {
        let t: Vec<Option<String>> = c.members.iter().map(|(a, col)| type_of(a, col)).collect();
        t.windows(2).any(|w| w[0] != w[1])
    }) {
        return None;
    }
    let mut plan = EcPlan {
        aliases: vec![a1.clone(), a2.clone()],
        ..EcPlan::default()
    };
    let mut consts = Vec::new();
    for class in &classes {
        if let Some(k) = class.constant {
            for (alias, col) in &class.members {
                let mut eq = pg_query::protobuf::AExpr {
                    kind: pg_query::protobuf::AExprKind::AexprOp as i32,
                    name: vec![pg_query::protobuf::Node {
                        node: Some(N::String(pg_query::protobuf::String { sval: "=".into() })),
                    }],
                    lexpr: Some(Box::new(pg_query::protobuf::Node {
                        node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                            fields: vec![pg_query::protobuf::Node {
                                node: Some(N::String(pg_query::protobuf::String {
                                    sval: col.clone(),
                                })),
                            }],
                            location: -1,
                        })),
                    })),
                    rexpr: Some(Box::new(k.clone())),
                    location: -1,
                };
                eq.location = -1;
                consts.push((
                    alias.clone(),
                    pg_query::protobuf::Node {
                        node: Some(N::AExpr(Box::new(eq))),
                    },
                ));
            }
            continue;
        }
        // Each side's own members, chained; the first on each side join.
        for alias in [a1, a2] {
            let mine: Vec<&(String, String)> =
                class.members.iter().filter(|(a, _)| a == alias).collect();
            for w in mine.windows(2) {
                plan.restrict
                    .push((alias.clone(), w[0].1.clone(), w[1].1.clone()));
            }
        }
        let first = |alias: &str| class.members.iter().find(|(a, _)| a == alias).cloned();
        if let (Some(x), Some(y)) = (first(a1), first(a2)) {
            plan.join_keys.push((x, y));
        }
    }
    Some((plan, absorbed, consts))
}

/// The aliases a WHERE conjunct is false or NULL for when their columns are
/// NULL (`find_nonnullable_rels`): a comparison's columns, under a cast or
/// not, and an `IS NOT NULL`'s. Anything else proves nothing here.
fn strict_aliases(
    n: &pg_query::protobuf::Node,
    owner: &dyn Fn(&str) -> Option<String>,
) -> Vec<String> {
    use pg_query::protobuf::{AExprKind, NullTestType};
    fn column(
        n: &pg_query::protobuf::Node,
        owner: &dyn Fn(&str) -> Option<String>,
        out: &mut Vec<String>,
    ) -> bool {
        match n.node.as_ref() {
            Some(N::ColumnRef(c)) => {
                let parts: Option<Vec<&str>> = c
                    .fields
                    .iter()
                    .map(|f| match f.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.as_str()),
                        _ => None,
                    })
                    .collect();
                let alias = match parts.as_deref() {
                    Some([q, _]) => Some(q.to_string()),
                    Some([name]) => owner(name),
                    _ => None,
                };
                if let Some(a) = alias {
                    out.push(a);
                }
                true
            }
            Some(N::AConst(_)) | Some(N::ParamRef(_)) => true,
            Some(N::TypeCast(t)) => t.arg.as_deref().is_some_and(|a| column(a, owner, out)),
            // Arithmetic is strict (`find_nonnullable_rels` descends into a
            // strict operator's arguments): NULL in, NULL out.
            Some(N::AExpr(e))
                if e.kind == AExprKind::AexprOp as i32
                    && matches!(crate::operator_name(e), Ok("+" | "-" | "*" | "/" | "%")) =>
            {
                e.lexpr.as_deref().is_none_or(|l| column(l, owner, out))
                    && e.rexpr.as_deref().is_some_and(|r| column(r, owner, out))
            }
            _ => false,
        }
    }
    let mut out = Vec::new();
    let strict = match n.node.as_ref() {
        Some(N::AExpr(e))
            if e.kind == AExprKind::AexprOp as i32
                && matches!(
                    crate::operator_name(e),
                    Ok("=" | "<>" | "<" | "<=" | ">" | ">=")
                ) =>
        {
            match (e.lexpr.as_deref(), e.rexpr.as_deref()) {
                (Some(l), Some(r)) => column(l, owner, &mut out) && column(r, owner, &mut out),
                _ => false,
            }
        }
        Some(N::NullTest(t)) if t.nulltesttype == NullTestType::IsNotNull as i32 => {
            t.arg.as_deref().is_some_and(|a| column(a, owner, &mut out))
        }
        _ => false,
    };
    if strict {
        out
    } else {
        Vec::new()
    }
}

/// The aliases of the FROM items below `item`: tables and subqueries, through
/// its joins.
fn subtree_aliases(item: &pg_query::protobuf::Node, out: &mut Vec<String>) {
    match item.node.as_ref() {
        Some(N::RangeVar(r)) => out.push(
            r.alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| r.relname.clone()),
        ),
        Some(N::RangeSubselect(r)) => {
            if let Some(a) = &r.alias {
                out.push(a.aliasname.clone());
            }
        }
        Some(N::RangeFunction(r)) => {
            if let Some(a) = &r.alias {
                out.push(a.aliasname.clone());
            }
        }
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                subtree_aliases(side, out);
            }
        }
        _ => {}
    }
}

/// Each stored table below `item`, by alias, with its definition; `false`
/// when anything else is there.
fn stored_tables(
    item: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    out: &mut Vec<(String, TableDef)>,
) -> bool {
    match item.node.as_ref() {
        // Column aliases (`t AS x(a, b)`) rename the table's columns.
        Some(N::RangeVar(r))
            if r.schemaname.is_empty()
                && r.alias.as_ref().is_none_or(|a| a.colnames.is_empty()) =>
        {
            let Some(def) = lookup(&r.relname) else {
                return false;
            };
            let alias = r
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| r.relname.clone());
            out.push((alias, def));
            true
        }
        Some(N::JoinExpr(j)) => {
            j.larg
                .as_deref()
                .is_some_and(|l| stored_tables(l, lookup, out))
                && j.rarg
                    .as_deref()
                    .is_some_and(|r| stored_tables(r, lookup, out))
        }
        _ => false,
    }
}

/// The table aliases below `item` that no outer join makes nullable.
/// Every table alias below `item`.
fn table_aliases(item: &pg_query::protobuf::Node, out: &mut Vec<String>) {
    match item.node.as_ref() {
        Some(N::RangeVar(r)) => out.push(
            r.alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| r.relname.clone()),
        ),
        Some(N::JoinExpr(j)) => {
            for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                table_aliases(side, out);
            }
        }
        _ => {}
    }
}

/// A LEFT join whose nullable side holds a `nonnullable` alias counts as
/// an inner one (`reduce_outer_joins`).
fn preserved_aliases(
    item: &pg_query::protobuf::Node,
    nullable: bool,
    nonnullable: &[String],
    out: &mut Vec<String>,
) {
    use pg_query::protobuf::JoinType;
    match item.node.as_ref() {
        Some(N::RangeVar(r)) if !nullable => out.push(
            r.alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .unwrap_or_else(|| r.relname.clone()),
        ),
        Some(N::JoinExpr(j)) => {
            let (l, r) = match JoinType::try_from(j.jointype) {
                Ok(JoinType::JoinInner) => (false, false),
                Ok(JoinType::JoinLeft) => {
                    let mut below = Vec::new();
                    if let Some(rarg) = j.rarg.as_deref() {
                        table_aliases(rarg, &mut below);
                    }
                    (false, !below.iter().any(|a| nonnullable.contains(a)))
                }
                // A RIGHT join's nullable side is its left, which the WHERE
                // can make non-null as it does a LEFT join's right.
                Ok(JoinType::JoinRight) => {
                    let mut below = Vec::new();
                    if let Some(larg) = j.larg.as_deref() {
                        table_aliases(larg, &mut below);
                    }
                    (!below.iter().any(|a| nonnullable.contains(a)), false)
                }
                // A FULL join's side stays nullable unless the WHERE proves
                // it non-null (`reduce_outer_joins` leaves a LEFT, RIGHT or
                // inner join, whose preserved sides those are).
                Ok(JoinType::JoinFull) => {
                    let proven = |side: Option<&pg_query::protobuf::Node>| {
                        let mut below = Vec::new();
                        if let Some(side) = side {
                            table_aliases(side, &mut below);
                        }
                        below.iter().any(|a| nonnullable.contains(a))
                    };
                    let (l_proven, r_proven) =
                        (proven(j.larg.as_deref()), proven(j.rarg.as_deref()));
                    (!l_proven, !r_proven)
                }
                _ => (true, true),
            };
            if let Some(larg) = j.larg.as_deref() {
                preserved_aliases(larg, nullable || l, nonnullable, out);
            }
            if let Some(rarg) = j.rarg.as_deref() {
                preserved_aliases(rarg, nullable || r, nonnullable, out);
            }
        }
        _ => {}
    }
}

fn split_and<'a>(n: &'a pg_query::protobuf::Node, out: &mut Vec<&'a pg_query::protobuf::Node>) {
    match n.node.as_ref() {
        Some(N::BoolExpr(b)) if b.boolop == pg_query::protobuf::BoolExprType::AndExpr as i32 => {
            for a in &b.args {
                split_and(a, out);
            }
        }
        _ => out.push(n),
    }
}

/// The one alias every column of `n` is qualified by, or belongs to
/// (`owner`, for an unqualified one), when `n` is a shape that may be
/// pushed.
fn single_alias(
    n: &pg_query::protobuf::Node,
    owner: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    match alias_of(n, owner, false) {
        Some((alias, false)) => Some(alias),
        _ => None,
    }
}

/// `single_alias`, also taking a conjunct that calls a volatile or stable
/// built-in or a user function over its one table: `(alias, true)` for one,
/// which must be MOVED into the leaf rather than copied -- PostgreSQL runs
/// a base relation's quals in its scan, once a row of that table (RP-019),
/// and a copy left in the outer WHERE would run it again per joined row.
fn movable_alias(
    n: &pg_query::protobuf::Node,
    owner: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, bool)> {
    alias_of(n, owner, true)
}

fn alias_of(
    n: &pg_query::protobuf::Node,
    owner: &dyn Fn(&str) -> Option<String>,
    movable: bool,
) -> Option<(String, bool)> {
    fn walk(
        n: &pg_query::protobuf::Node,
        alias: &mut Option<String>,
        moved: &std::cell::Cell<bool>,
        owner: &dyn Fn(&str) -> Option<String>,
        movable: bool,
    ) -> bool {
        let mut rec = |x: &pg_query::protobuf::Node| walk(x, alias, moved, owner, movable);
        match n.node.as_ref() {
            Some(N::ColumnRef(c)) => {
                let parts: Option<Vec<&str>> = c
                    .fields
                    .iter()
                    .map(|f| match f.node.as_ref() {
                        Some(N::String(s)) => Some(s.sval.as_str()),
                        _ => None,
                    })
                    .collect();
                let qualifier = match parts.as_deref() {
                    Some([q, _]) => Some(q.to_string()),
                    Some([name]) => owner(name),
                    _ => None,
                };
                match qualifier {
                    Some(q) => match alias {
                        Some(a) => *a == q,
                        None => {
                            *alias = Some(q);
                            true
                        }
                    },
                    _ => false,
                }
            }
            Some(N::AConst(_)) | Some(N::ParamRef(_)) => true,
            Some(N::TypeCast(t)) => t.arg.as_deref().is_some_and(&mut rec),
            Some(N::AExpr(e)) => {
                e.lexpr.as_deref().is_none_or(&mut rec) && e.rexpr.as_deref().is_none_or(&mut rec)
            }
            Some(N::BoolExpr(b)) => b.args.iter().all(&mut rec),
            Some(N::NullTest(t)) => t.arg.as_deref().is_some_and(&mut rec),
            Some(N::BooleanTest(t)) => t.arg.as_deref().is_some_and(&mut rec),
            Some(N::List(l)) => l.items.iter().all(&mut rec),
            Some(N::AArrayExpr(a)) => a.elements.iter().all(&mut rec),
            // An IMMUTABLE built-in: the pushed copy and the one left in the
            // outer WHERE give the same answer, so evaluating it twice is
            // harmless. A volatile or stable one, or a user function (which
            // may shadow the built-in), is only MOVED (`movable_alias`). An
            // aggregate or window call, a set-returning one, and a correlated
            // subquery's call stay in the outer WHERE only.
            Some(N::FuncCall(f)) => {
                let plain = f.over.is_none()
                    && f.agg_filter.is_none()
                    && f.agg_order.is_empty()
                    && !f.agg_star
                    && !f.agg_distinct;
                let Some(name) = func_name(f).filter(|_| plain) else {
                    return false;
                };
                let user = crate::correlated::user_function_for(&name, &f.args);
                let immutable =
                    user.is_none() && crate::correlated::immutable_builtin(&name, f.args.len());
                if !immutable {
                    let callable = match &user {
                        Some(u) => !u.returns_set,
                        None => {
                            crate::funcsig::is_builtin_function_name(&name)
                                && !crate::funcsig::always_returns_set(&name)
                                && aggregate_func(&name, false).is_none()
                                && !crate::correlated::is_correlated_name(&name)
                        }
                    };
                    if !(movable && callable) {
                        return false;
                    }
                    moved.set(true);
                }
                f.args.iter().all(&mut rec)
            }
            _ => false,
        }
    }
    let mut alias = None;
    let moved = std::cell::Cell::new(false);
    walk(n, &mut alias, &moved, owner, movable)
        .then_some(alias)
        .flatten()
        .map(|a| (a, moved.get()))
}

/// `a.x` -> `x`, for a conjunct moved into `a`'s own `SELECT * FROM t`.
fn strip_qualifier(n: &mut pg_query::protobuf::Node) {
    let _ = walk_expr(n, &mut |x| {
        if let Some(N::ColumnRef(c)) = x.node.as_mut() {
            if c.fields.len() == 2 {
                c.fields.remove(0);
            }
        }
        Ok(())
    });
}

/// The conjuncts pushed to `alias`, AND-ed.
fn pushed_filter(alias: &str) -> Option<pg_query::protobuf::Node> {
    let mine: Vec<pg_query::protobuf::Node> = PUSHDOWN.with(|p| {
        p.borrow()
            .iter()
            .filter(|(a, _)| a == alias)
            .map(|(_, c)| c.clone())
            .collect()
    });
    match mine.len() {
        0 => None,
        1 => mine.into_iter().next(),
        _ => Some(pg_query::protobuf::Node {
            node: Some(N::BoolExpr(Box::new(pg_query::protobuf::BoolExpr {
                boolop: pg_query::protobuf::BoolExprType::AndExpr as i32,
                args: mine,
                location: -1,
                ..Default::default()
            }))),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The aliases each WHERE conjunct of `sql` is pushed to.
    fn pushed_to(sql: &str) -> Vec<String> {
        let parsed = pg_query::parse(sql).unwrap();
        let stmt = parsed.protobuf.stmts[0].stmt.as_ref().unwrap();
        let Some(N::SelectStmt(s)) = stmt.node.as_ref() else {
            panic!("not a SELECT: {sql}");
        };
        let lookup = |name: &str| -> Option<TableDef> {
            let cols = match name {
                "a" => vec![
                    Column::new("id", "int4", true),
                    Column::new("v", "int4", false),
                ],
                "b" => vec![
                    Column::new("id", "int4", false),
                    Column::new("a_id", "int4", false),
                    Column::new("w", "text", false),
                ],
                _ => return None,
            };
            Some(TableDef::new(name, cols))
        };
        let (pushed, _, _, _, _) = pushdown_filters(s, &lookup);
        pushed.into_iter().map(|(alias, _)| alias).collect()
    }

    #[test]
    fn an_immutable_builtin_call_is_pushed_to_its_leaf() {
        // RP-019: a conjunct calling an IMMUTABLE built-in reads one alias,
        // so its leaf applies it (and can reach an index for it).
        assert_eq!(
            pushed_to("select 1 from a join b on b.a_id = a.id where lower(b.w) = 'w1'"),
            vec!["b"]
        );
        assert_eq!(
            pushed_to("select 1 from a join b on b.a_id = a.id where abs(a.v - 3) = 1"),
            vec!["a"]
        );
        // Unqualified, but only `b` has `w`.
        assert_eq!(
            pushed_to("select 1 from a join b on b.a_id = a.id where upper(w) like 'W1%'"),
            vec!["b"]
        );
    }

    #[test]
    fn a_right_joins_left_side_is_nullable_until_the_where_says_otherwise() {
        // `reduce_outer_joins`: a strict condition on a RIGHT join's left
        // side makes the join inner, and that side's scan applies it; the
        // preserved side's conjuncts are pushed either way.
        assert_eq!(
            pushed_to("select 1 from a right join b on b.a_id = a.id where a.v > 3"),
            vec!["a"]
        );
        assert_eq!(
            pushed_to("select 1 from a right join b on b.a_id = a.id where b.w = 'x'"),
            vec!["b"]
        );
        assert!(
            pushed_to("select 1 from a right join b on b.a_id = a.id where a.v is null").is_empty()
        );
    }

    /// The positions among the WHERE's conjuncts of those MOVED to a leaf.
    fn moved_in(sql: &str) -> Vec<usize> {
        let parsed = pg_query::parse(sql).unwrap();
        let stmt = parsed.protobuf.stmts[0].stmt.as_ref().unwrap();
        let Some(N::SelectStmt(s)) = stmt.node.as_ref() else {
            panic!("not a SELECT: {sql}");
        };
        let lookup = |name: &str| -> Option<TableDef> {
            let cols = match name {
                "a" => vec![
                    Column::new("id", "int4", true),
                    Column::new("v", "int4", false),
                ],
                "b" => vec![
                    Column::new("id", "int4", false),
                    Column::new("a_id", "int4", false),
                ],
                _ => return None,
            };
            Some(TableDef::new(name, cols))
        };
        pushdown_filters(s, &lookup).4
    }

    #[test]
    fn a_call_that_may_not_be_immutable_is_moved_not_copied() {
        // Volatile: it runs in a's scan, once a row of a (RP-019), and
        // leaves the outer WHERE, where it would run again per joined row.
        let volatile = "select 1 from a join b on b.a_id = a.id where random() < a.v";
        assert_eq!(pushed_to(volatile), vec!["a"]);
        assert_eq!(moved_in(volatile), vec![0]);
        // Stable, not immutable: moved too.
        let stable = "select 1 from a join b on b.a_id = a.id \
                      where b.id > 0 and a.v < extract(day from now())";
        assert_eq!(pushed_to(stable), vec!["b", "a"]);
        assert_eq!(moved_in(stable), vec![1]);
        // Only into a leaf that takes a filter: a renamed table does not.
        assert!(
            pushed_to("select 1 from a x(i, w) join b on b.a_id = x.i where random() < x.w")
                .is_empty()
        );
        // An aggregate cannot appear in a WHERE, and is never pushed.
        assert!(
            pushed_to("select 1 from a join b on b.a_id = a.id where a.v = count(*)").is_empty()
        );
        // A call over two aliases is a join clause, not a leaf filter.
        assert!(
            pushed_to("select 1 from a join b on b.a_id = a.id where abs(a.v - b.id) = 1")
                .is_empty()
        );
        // The nullable side of a LEFT join keeps its filter above the join.
        assert!(
            pushed_to("select 1 from a left join b on b.a_id = a.id where lower(b.w) is null")
                .is_empty()
        );
    }
}
