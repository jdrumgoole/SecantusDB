//! Operator resolution by type, as PostgreSQL's parse analysis does it.
//!
//! A comparison between two values of different type CATEGORIES -- a text
//! column against an integer, a date against a number, a boolean against an
//! integer -- has no operator in `pg_operator` and no implicit cast to find
//! one through, so PostgreSQL refuses it at plan time with `42883 operator
//! does not exist: text = integer`. The lowering to MQL would instead compare
//! the two values and match nothing, answering an empty result where the
//! client should have been told its query is wrong.
//!
//! Only operands whose type is KNOWN statically are checked: a column of a
//! table in the FROM list, a typed constant, a cast, a declared parameter.
//! An untyped string literal is `unknown` and takes the other side's type,
//! so it is never a mismatch; anything else this cannot type is skipped.

use super::*;

mod regclass;
mod sortable;

pub(crate) use sortable::using_to_direction;

/// The type category of a built-in type, by oid; `None` for a type whose
/// operators this does not reason about (a domain, an enum, `oid`, arrays).
fn category(ty: &str) -> Option<&'static str> {
    Some(match pgtypes::oid_of_name(ty)? {
        20 | 21 | 23 | 700 | 701 | 1700 => "numeric",
        18 | 19 | 25 | 1042 | 1043 => "string",
        1082 | 1114 | 1184 => "datetime",
        16 => "bool",
        17 => "bytea",
        1186 => "interval",
        2950 => "uuid",
        3802 => "jsonb",
        _ => return None,
    })
}

/// The relations a statement's FROM list names, by the name a column
/// qualifier uses; `complete` when every FROM item is one of them, so an
/// unqualified column resolves among them alone. A subquery's scope has
/// its enclosing query's as `parent`, where a name it does not have resolves.
/// A CTE in scope: its name and, when it is a plain SELECT, its query and
/// the column names written after its name (`with t(x, y) as ...`).
type Cte = (
    String,
    Option<(
        pg_query::protobuf::SelectStmt,
        Vec<pg_query::protobuf::Node>,
    )>,
);

/// The catalog: a relation's definition by name.
type Lookup<'p> = &'p dyn Fn(&str) -> Option<TableDef>;

struct Scope<'p> {
    tables: Vec<(String, TableDef)>,
    complete: bool,
    /// A FROM relation the catalog lookup did not find -- missing, or one
    /// this check does not type. PostgreSQL resolves the FROM list before
    /// the clauses, so a missing relation's 42P01 comes first.
    unresolved: bool,
    parent: Option<&'p Scope<'p>>,
    /// The catalog and CTEs in view, to type a scalar subquery's column.
    lookup: Option<Lookup<'p>>,
    ctes: Vec<Cte>,
    /// A USING or NATURAL join's merged columns, with the range of `tables`
    /// (the join's leaves) whose own columns of that name it hides from an
    /// unqualified reference: PostgreSQL resolves one through the join's
    /// RTE, whose merged column has the sides' common type.
    merged: Vec<(Column, std::ops::Range<usize>)>,
    /// A USING join's alias (`join b using (x) as u`) with the merged
    /// columns it names.
    using_aliases: Vec<(String, Vec<Column>)>,
    /// The row type a whole-row reference to an entry of `tables` has, by
    /// the entry's index: a table's or a view's own composite type, or
    /// `record` for a FROM subquery or a CTE (an `RTE_SUBQUERY`'s whole-row
    /// Var is RECORD). An entry not listed is not typed.
    rowtypes: Vec<(usize, String)>,
}

impl<'p> Scope<'p> {
    fn new(
        items: &[pg_query::protobuf::Node],
        ctes: &[Cte],
        lookup: &'p dyn Fn(&str) -> Option<TableDef>,
        parent: Option<&'p Scope<'p>>,
    ) -> Scope<'p> {
        let mut scope = Scope {
            tables: Vec::new(),
            complete: true,
            unresolved: false,
            parent,
            lookup: Some(lookup),
            ctes: ctes.to_vec(),
            merged: Vec::new(),
            using_aliases: Vec::new(),
            rowtypes: Vec::new(),
        };
        for item in items {
            scope.add(item, ctes, lookup);
        }
        scope
    }

    /// Does this scope, or one it is nested in, have a FROM relation the
    /// lookup did not find?
    fn relation_unresolved(&self) -> bool {
        self.unresolved || self.parent.is_some_and(|p| p.relation_unresolved())
    }

    fn add(
        &mut self,
        item: &pg_query::protobuf::Node,
        ctes: &[Cte],
        lookup: &dyn Fn(&str) -> Option<TableDef>,
    ) {
        let is_cte = |r: &pg_query::protobuf::RangeVar| {
            r.schemaname.is_empty() && ctes.iter().any(|(n, _)| *n == r.relname)
        };
        match item.node.as_ref() {
            // A CTE: typed from its query, as a FROM subquery is.
            Some(N::RangeVar(r)) if is_cte(r) => {
                let def = ctes
                    .iter()
                    .rev()
                    .find(|(n, _)| *n == r.relname)
                    .and_then(|(_, q)| q.as_ref());
                let alias = r
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .filter(|a| !a.is_empty())
                    .unwrap_or_else(|| r.relname.clone());
                let aliased = r
                    .alias
                    .as_ref()
                    .map(|a| a.colnames.clone())
                    .unwrap_or_default();
                match def {
                    // The FROM alias's names first, then the CTE's own.
                    Some((q, named)) => {
                        let colnames: Vec<_> = (0..aliased.len().max(named.len()))
                            .filter_map(|i| aliased.get(i).or(named.get(i)).cloned())
                            .collect();
                        let at = self.tables.len();
                        self.add_derived(alias, &colnames, q, ctes, lookup, None);
                        self.row_typed(at, "record");
                    }
                    None => self.complete = false,
                }
            }
            Some(N::RangeSubselect(rs)) => {
                let alias = rs
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_default();
                let colnames = rs
                    .alias
                    .as_ref()
                    .map(|a| a.colnames.clone())
                    .unwrap_or_default();
                match rs.subquery.as_deref().and_then(|n| n.node.as_ref()) {
                    // A LATERAL subquery sees the FROM items before it, and
                    // the queries around this one (`transformFromClauseItem`
                    // puts the left items in its namespace, lateral-only).
                    Some(N::SelectStmt(q)) if !alias.is_empty() && rs.lateral => {
                        let left = self.snapshot();
                        let at = self.tables.len();
                        self.add_derived(alias, &colnames, q, ctes, lookup, Some(&left));
                        self.row_typed(at, "record");
                    }
                    Some(N::SelectStmt(q)) if !alias.is_empty() => {
                        let at = self.tables.len();
                        self.add_derived(alias, &colnames, q, ctes, lookup, None);
                        self.row_typed(at, "record");
                    }
                    _ => self.complete = false,
                }
            }
            Some(N::RangeVar(r)) if !is_cte(r) => {
                let name = if r.schemaname.is_empty() || r.schemaname == "public" {
                    r.relname.clone()
                } else {
                    format!("{}.{}", r.schemaname, r.relname)
                };
                match lookup(&name) {
                    Some(def) => {
                        let alias = r
                            .alias
                            .as_ref()
                            .map(|a| a.aliasname.clone())
                            .filter(|a| !a.is_empty())
                            .unwrap_or_else(|| r.relname.clone());
                        self.rowtypes.push((self.tables.len(), name.clone()));
                        self.tables.push((alias, def));
                    }
                    // A view: an RTE_SUBQUERY over its stored query, whose
                    // columns `DefineView` typed from that query, analysed
                    // on its own (no CTE of this statement reaches it).
                    None => match view_select(r) {
                        Some(q) => {
                            let alias = r
                                .alias
                                .as_ref()
                                .map(|a| a.aliasname.clone())
                                .filter(|a| !a.is_empty())
                                .unwrap_or_else(|| r.relname.clone());
                            let colnames = r
                                .alias
                                .as_ref()
                                .map(|a| a.colnames.clone())
                                .unwrap_or_default();
                            // A view whose definition reaches itself (only
                            // through OR REPLACE) is the planner's to refuse.
                            let depth = VIEW_DEPTH.with(|d| d.get());
                            if depth > 32 {
                                self.complete = false;
                            } else {
                                VIEW_DEPTH.with(|d| d.set(depth + 1));
                                let at = self.tables.len();
                                self.add_derived(alias, &colnames, &q, &[], lookup, None);
                                self.row_typed(at, &name);
                                VIEW_DEPTH.with(|d| d.set(depth));
                            }
                        }
                        None => {
                            self.complete = false;
                            self.unresolved = true;
                        }
                    },
                }
            }
            // A function in FROM: typed by planning `SELECT * FROM f(...)`,
            // its columns named by the alias list, or -- a one-column
            // function -- by the bare alias.
            Some(N::RangeFunction(rf)) if !rf.lateral => {
                let alias = rf
                    .alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .filter(|a| !a.is_empty())
                    .or_else(|| crate::joins::range_function_name(rf));
                let mut bare = rf.clone();
                bare.alias = None;
                let probe = pg_query::protobuf::SelectStmt {
                    target_list: vec![pg_query::protobuf::Node {
                        node: Some(N::ResTarget(Box::new(pg_query::protobuf::ResTarget {
                            val: Some(Box::new(crate::star_target_node())),
                            location: -1,
                            ..Default::default()
                        }))),
                    }],
                    from_clause: vec![pg_query::protobuf::Node {
                        node: Some(N::RangeFunction(bare)),
                    }],
                    limit_option: pg_query::protobuf::LimitOption::Default as i32,
                    op: pg_query::protobuf::SetOperation::SetopNone as i32,
                    ..Default::default()
                };
                // Only the columns' TYPES are wanted, which the function's
                // declaration gives: the probe must not CALL it, or a body
                // that writes writes once more than the statement runs it.
                let def = crate::correlated::without_calls(|| {
                    crate::plan_select(&probe, lookup, &[])
                        .and_then(|p| crate::sub_plan_def(&p, lookup))
                });
                match (alias, def) {
                    (Some(alias), Ok(mut def)) => {
                        let names: Vec<String> = rf
                            .alias
                            .as_ref()
                            .map(|a| {
                                a.colnames
                                    .iter()
                                    .filter_map(|n| match n.node.as_ref() {
                                        Some(N::String(s)) => Some(s.sval.clone()),
                                        _ => None,
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        if names.is_empty() && def.columns.len() == 1 {
                            def.columns[0].name = alias.clone();
                        }
                        for (c, n) in def.columns.iter_mut().zip(names) {
                            c.name = n;
                        }
                        self.tables.push((alias, def));
                    }
                    _ => self.complete = false,
                }
            }
            Some(N::JoinExpr(j)) => {
                let start = self.tables.len();
                if let Some(l) = j.larg.as_deref() {
                    self.add(l, ctes, lookup);
                }
                if let Some(r) = j.rarg.as_deref() {
                    self.add(r, ctes, lookup);
                }
                // A USING / NATURAL join merges columns: an unqualified name
                // reads the merged one, typed by `select_common_type` over
                // the sides', not either side's own. Only when every leaf of
                // the join was typed, one table each, so the leaves line up.
                let typed = self.tables.len() - start == side_leaves(j);
                let columns = typed
                    .then(|| {
                        let mut leaves = self.tables[start..].iter().map(|(_, d)| d);
                        let sides = j
                            .larg
                            .as_deref()
                            .and_then(|l| item_columns(l, &mut leaves))
                            .zip(j.rarg.as_deref().and_then(|r| item_columns(r, &mut leaves)));
                        sides.and_then(|(l, r)| merge_using(j, l, r))
                    })
                    .flatten();
                if j.is_natural || !j.using_clause.is_empty() {
                    if let Some((n, cols)) = &columns {
                        let range = start..self.tables.len();
                        self.merged
                            .extend(cols.iter().take(*n).map(|c| (c.clone(), range.clone())));
                        if let Some(u) = &j.join_using_alias {
                            self.using_aliases
                                .push((u.aliasname.clone(), cols[..*n].to_vec()));
                        }
                    }
                }
                // `(a join b) as j(x, ...)`: one entry, named `j`, whose
                // columns are those `*` lists, renamed in order; the sides'
                // entries and names are hidden behind it.
                if let Some(alias) = &j.alias {
                    match columns {
                        Some((_, mut cols)) if alias.colnames.len() <= cols.len() => {
                            for (c, n) in cols.iter_mut().zip(names_of_list(&alias.colnames)) {
                                c.name = n;
                            }
                            self.tables.truncate(start);
                            self.rowtypes.retain(|(i, _)| *i < start);
                            self.merged.retain(|(_, r)| r.start < start);
                            self.using_aliases.clear();
                            self.tables.push((
                                alias.aliasname.clone(),
                                TableDef::new(&alias.aliasname, cols),
                            ));
                        }
                        _ => self.complete = false,
                    }
                }
            }
            _ => self.complete = false,
        }
    }

    /// The derived table `add_derived` just added at `at`, if it added one,
    /// has whole rows of type `ty`.
    fn row_typed(&mut self, at: usize, ty: &str) {
        if self.tables.len() == at + 1 {
            self.rowtypes.push((at, ty.to_string()));
        }
    }

    /// The row type of a whole-row reference -- `t` naming a FROM item and
    /// no column, or `t.*` -- as `transformWholeRowRef` types its Var, or
    /// `None` when it is not one or this cannot tell. A column of that name
    /// is the column (`colNameToVar` comes first).
    fn whole_row_type(&self, c: &pg_query::protobuf::ColumnRef) -> Option<String> {
        let (name, star) = match c.fields.as_slice() {
            [f] => match f.node.as_ref() {
                Some(N::String(s)) => (s.sval.as_str(), false),
                _ => return None,
            },
            [f, g] if matches!(g.node.as_ref(), Some(N::AStar(_))) => match f.node.as_ref() {
                Some(N::String(s)) => (s.sval.as_str(), true),
                _ => return None,
            },
            _ => return None,
        };
        // A bare name is a column first, at every level out
        // (`transformColumnRef` tries `colNameToVar` before a relation).
        if !star && self.knows(name) != Some(false) {
            return None;
        }
        let mut hits = self
            .tables
            .iter()
            .enumerate()
            .filter(|(_, (a, _))| a == name);
        match (hits.next(), hits.next()) {
            (Some((i, _)), None) => self
                .rowtypes
                .iter()
                .find(|(at, _)| *at == i)
                .map(|(_, t)| t.clone()),
            (None, _) if self.complete => self.parent?.whole_row_type(c),
            _ => None,
        }
    }

    /// This scope as it stands, for a LATERAL item to see the ones before it.
    fn snapshot(&self) -> Scope<'p> {
        Scope {
            tables: self.tables.clone(),
            complete: self.complete,
            unresolved: self.unresolved,
            parent: self.parent,
            lookup: self.lookup,
            ctes: self.ctes.clone(),
            merged: self.merged.clone(),
            using_aliases: self.using_aliases.clone(),
            rowtypes: self.rowtypes.clone(),
        }
    }

    /// A derived table -- a FROM subquery or a CTE -- whose columns are
    /// typed from its select list. A column this cannot type is still a
    /// column (so an unqualified name resolves among the known ones); it is
    /// just never judged. `parent` is what a LATERAL one sees around it.
    fn add_derived(
        &mut self,
        alias: String,
        colnames: &[pg_query::protobuf::Node],
        q: &pg_query::protobuf::SelectStmt,
        ctes: &[Cte],
        lookup: &dyn Fn(&str) -> Option<TableDef>,
        parent: Option<&Scope>,
    ) {
        if q.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
            self.complete = false;
            return;
        }
        let inner = Scope::new(&q.from_clause, ctes, lookup, parent);
        let named = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        };
        // `(VALUES ...) v(a, b)`: column k is the rows' common type at k
        // (`select_common_type`), named by the alias list or `columnK`.
        if !q.values_lists.is_empty() {
            let rows: Vec<&[pg_query::protobuf::Node]> = q
                .values_lists
                .iter()
                .filter_map(|r| match r.node.as_ref() {
                    Some(N::List(l)) => Some(l.items.as_slice()),
                    _ => None,
                })
                .collect();
            let width = rows.first().map_or(0, |r| r.len());
            if rows.len() != q.values_lists.len() || rows.iter().any(|r| r.len() != width) {
                self.complete = false;
                return;
            }
            let columns = (0..width)
                .map(|k| {
                    let name = colnames
                        .get(k)
                        .and_then(named)
                        .unwrap_or_else(|| format!("column{}", k + 1));
                    let parts: Vec<&pg_query::protobuf::Node> =
                        rows.iter().map(|r| &r[k]).collect();
                    let ty = common_type(&parts, &inner).unwrap_or_default();
                    Column::new(&name, &ty, true)
                })
                .collect();
            self.tables
                .push((alias.clone(), TableDef::new(&alias, columns)));
            return;
        }
        let mut columns = Vec::new();
        for t in &q.target_list {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                self.complete = false;
                return;
            };
            let Some(val) = rt.val.as_deref() else {
                self.complete = false;
                return;
            };
            // `*` / `t.*`: the FROM items' columns, in order
            // (`ExpandColumnRefStar`), named by the alias list by position.
            if let Some(N::ColumnRef(c)) = val.node.as_ref() {
                if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_)))) {
                    let Some(expanded) = star_columns(c, &inner, &q.from_clause) else {
                        self.complete = false;
                        return;
                    };
                    for col in expanded {
                        let name = colnames
                            .get(columns.len())
                            .and_then(named)
                            .unwrap_or_else(|| col.name.clone());
                        columns.push(Column::new(&name, &col.pg_type, true));
                    }
                    continue;
                }
            }
            // The output position, past what a `*` before it expanded to.
            let i = columns.len();
            let name = colnames
                .get(i)
                .and_then(named)
                .or_else(|| (!rt.name.is_empty()).then(|| rt.name.clone()))
                .or_else(|| match val.node.as_ref() {
                    Some(N::ColumnRef(c)) => c.fields.last().and_then(named),
                    Some(N::FuncCall(f)) => func_name(f),
                    _ => None,
                })
                .unwrap_or_else(|| "?column?".to_string());
            // An untyped literal or NULL is `text` to the query around
            // this one (`resolveTargetListUnknowns`, which a FROM
            // subquery, a CTE and a view are analysed with).
            let ty = if is_untyped_literal(val) {
                "text".to_string()
            } else {
                operand_type(val, &inner).unwrap_or_default()
            };
            columns.push(Column::new(&name, &ty, true));
        }
        // A name the derived table has twice is ambiguous to a reference
        // (42702), not a column of either type: judged by neither.
        let names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        for c in &mut columns {
            if names.iter().filter(|n| **n == c.name).count() > 1 {
                c.pg_type.clear();
            }
        }
        self.tables
            .push((alias.clone(), TableDef::new(&alias, columns)));
    }

    /// Does a bare column name resolve in this scope or an enclosing one?
    /// `Some(false)` only when every scope on the way is complete.
    fn knows(&self, name: &str) -> Option<bool> {
        if self.tables.iter().any(|(_, d)| d.column(name).is_some()) {
            return Some(true);
        }
        if !self.complete {
            return None;
        }
        match self.parent {
            Some(p) => p.knows(name),
            None => Some(false),
        }
    }

    /// Is `name` the reference name of a FROM item here or in an enclosing
    /// scope? A bare name no column has is then that item's whole row, as
    /// `transformColumnRef` falls back to `refnameNamespaceItem` (RP-532).
    fn names_item(&self, name: &str) -> bool {
        self.tables.iter().any(|(a, _)| a == name)
            || self.using_aliases.iter().any(|(a, _)| a == name)
            || self.parent.is_some_and(|p| p.names_item(name))
    }

    fn column_type(&self, c: &pg_query::protobuf::ColumnRef) -> Option<String> {
        self.column_type_raw(c).filter(|t| !t.is_empty())
    }

    fn column_type_raw(&self, c: &pg_query::protobuf::ColumnRef) -> Option<String> {
        let parts: Vec<&str> = c
            .fields
            .iter()
            .map(|f| match f.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.as_str()),
                _ => None,
            })
            .collect::<Option<_>>()?;
        match parts.as_slice() {
            [col] => {
                // A join's merged column hides its leaves' own; of merged
                // columns nested in one another, the outermost join's is
                // the one named.
                let merged: Vec<&(Column, std::ops::Range<usize>)> =
                    self.merged.iter().filter(|(c, _)| c.name == *col).collect();
                let hidden = |i: usize| merged.iter().any(|(_, r)| r.contains(&i));
                let outermost = merged.iter().filter(|(_, r)| {
                    !merged
                        .iter()
                        .any(|(_, o)| o != r && o.start <= r.start && r.end <= o.end)
                });
                let mut found = outermost.map(|(c, _)| c.pg_type.clone()).chain(
                    self.tables
                        .iter()
                        .enumerate()
                        .filter(|(i, _)| !hidden(*i))
                        .filter_map(|(_, (_, d))| d.column(col).map(|c| c.pg_type.clone())),
                );
                match found.next() {
                    // Named by more than one side, or possibly by a FROM item
                    // this cannot see into: not ours to judge.
                    Some(first) => (self.complete && found.next().is_none()).then_some(first),
                    // Not a column here: an outer reference.
                    None if self.complete => self.parent?.column_type(c),
                    None => None,
                }
            }
            [q, col] => match self.tables.iter().find(|(a, _)| a == q) {
                Some((_, d)) => d.column(col).map(|c| c.pg_type.clone()),
                None => match self.using_aliases.iter().find(|(a, _)| a == q) {
                    Some((_, cols)) => cols
                        .iter()
                        .find(|c| c.name == *col)
                        .map(|c| c.pg_type.clone()),
                    None if self.complete => self.parent?.column_type(c),
                    None => None,
                },
            },
            _ => None,
        }
    }
}

/// What a `*` or `t.*` in a derived table's select list expands to: every
/// column of the FROM items in order, or of the one named
/// (`ExpandColumnRefStar`). `None` when this cannot list them exactly: a
/// FROM item it does not see into, or a join whose columns it cannot tell.
fn star_columns(
    c: &pg_query::protobuf::ColumnRef,
    inner: &Scope,
    from: &[pg_query::protobuf::Node],
) -> Option<Vec<Column>> {
    if !inner.complete {
        return None;
    }
    let parts: Vec<Option<&str>> = c
        .fields
        .iter()
        .map(|f| match f.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    match parts.as_slice() {
        [None] => {
            // A complete scope holds one table per FROM leaf, in order.
            let mut leaves = inner.tables.iter().map(|(_, d)| d);
            let mut out = Vec::new();
            for item in from {
                out.extend(item_columns(item, &mut leaves)?);
            }
            Some(out)
        }
        [Some(q), None] => Some(inner.tables.iter().find(|(a, _)| a == q)?.1.columns.clone()),
        _ => None,
    }
}

/// A FROM item's columns as `*` lists them (`expandRTE`). A USING or
/// NATURAL join's merged columns come first, once, then each side's others
/// (`transformFromClauseItem`); a merged column is typed only when both
/// sides agree, as `select_common_type` would otherwise choose.
fn item_columns(
    item: &pg_query::protobuf::Node,
    leaves: &mut dyn Iterator<Item = &TableDef>,
) -> Option<Vec<Column>> {
    let Some(N::JoinExpr(j)) = item
        .node
        .as_ref()
        .filter(|n| !matches!(n, N::JoinExpr(j) if j.alias.is_some()))
    else {
        // A table, or an aliased JOIN, which `Scope::add` gave one entry.
        return Some(leaves.next()?.columns.clone());
    };
    let left = item_columns(j.larg.as_deref()?, leaves)?;
    let right = item_columns(j.rarg.as_deref()?, leaves)?;
    Some(merge_using(j, left, right)?.1)
}

/// A join's columns from its sides' (`transformFromClauseItem`): the merged
/// USING / NATURAL columns first, once each, typed by `select_common_type`
/// over the two sides' (untyped when they are not of one category, which is
/// PostgreSQL's 42804 at the join), then each side's others. With how many
/// are merged. `None` when a merged name is not one column on each side.
fn merge_using(
    j: &pg_query::protobuf::JoinExpr,
    left: Vec<Column>,
    right: Vec<Column>,
) -> Option<(usize, Vec<Column>)> {
    let using: Vec<String> = if j.is_natural {
        left.iter()
            .filter(|l| right.iter().any(|r| r.name == l.name))
            .map(|l| l.name.clone())
            .collect()
    } else {
        j.using_clause
            .iter()
            .map(|n| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            })
            .collect::<Option<_>>()?
    };
    let mut out = Vec::new();
    for u in &using {
        let one = |side: &[Column]| {
            let mut it = side.iter().filter(|c| c.name == *u);
            match (it.next(), it.next()) {
                (Some(c), None) => Some(c.pg_type.clone()),
                _ => None,
            }
        };
        let (l, r) = (one(&left)?, one(&right)?);
        let ty = if l.is_empty() || r.is_empty() {
            String::new()
        } else {
            select_common(&l, &r).unwrap_or_default()
        };
        out.push(Column::new(u, &ty, true));
    }
    let merged = out.len();
    let others = |side: Vec<Column>| side.into_iter().filter(|c| !using.contains(&c.name));
    out.extend(others(left));
    out.extend(others(right));
    Some((merged, out))
}

/// How many FROM leaves a FROM item holds: one, or a join's sides' sum.
fn leaf_count(item: &pg_query::protobuf::Node) -> usize {
    match item.node.as_ref() {
        Some(N::JoinExpr(j)) if j.alias.is_none() => side_leaves(j),
        _ => 1,
    }
}

/// How many entries a join's two sides hold in `Scope::tables`.
fn side_leaves(j: &pg_query::protobuf::JoinExpr) -> usize {
    j.larg.as_deref().map_or(0, leaf_count) + j.rarg.as_deref().map_or(0, leaf_count)
}

/// The names of an alias's column list.
fn names_of_list(list: &[pg_query::protobuf::Node]) -> Vec<String> {
    list.iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.clone()),
            _ => None,
        })
        .collect()
}

thread_local! {
    /// How deep `Scope::add` is in views' definitions.
    static VIEW_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The stored query of a user view `r` names, when it is one.
fn view_select(r: &pg_query::protobuf::RangeVar) -> Option<pg_query::protobuf::SelectStmt> {
    if !r.catalogname.is_empty() || !(r.schemaname.is_empty() || r.schemaname == "public") {
        return None;
    }
    let definition = crate::view_definition(&r.relname)?;
    match pg_query::parse(&definition)
        .ok()?
        .protobuf
        .stmts
        .first()?
        .stmt
        .as_ref()?
        .node
        .as_ref()?
    {
        N::SelectStmt(q) => Some((**q).clone()),
        _ => None,
    }
}

/// The type `select_common_type` resolves two typed inputs to, in that
/// order. Its loop keeps the first unless that one is not its category's
/// preferred type (`text`, `float8`) and casts implicitly to the second but
/// not back: so numbers climb `int2` -> `int4` -> `int8` -> `numeric` ->
/// `float4` -> `float8`, and among strings a leading `text` stays, `name`
/// takes over from `varchar` / `bpchar` (each casts to it, it to neither),
/// and otherwise the first stays, as `text`, `varchar` and `bpchar` cast
/// each way. `"char"` meets none of them; `None` across categories.
pub(crate) fn select_common(o: &str, t: &str) -> Option<String> {
    if o == t {
        return Some(o.to_string());
    }
    let (po, pt) = (pgtypes::oid_of_name(o)?, pgtypes::oid_of_name(t)?);
    if po == pt {
        return Some(o.to_string());
    }
    match (category(o)?, category(t)?) {
        ("numeric", "numeric") => {
            let rank = |oid: i64| [21, 23, 20, 1700, 700, 701].iter().position(|r| *r == oid);
            Some(if rank(pt)? > rank(po)? { t } else { o }.to_string())
        }
        ("string", "string") => {
            if po == 18 || pt == 18 {
                None
            } else if po != 25 && pt == 19 {
                Some(t.to_string())
            } else {
                Some(o.to_string())
            }
        }
        _ => None,
    }
}

/// The static type of `expr` in the FROM scope of `sel` (a subquery's
/// select-list expression), or `None` when it cannot be told or is
/// `unknown`.
pub(crate) fn type_in_select(
    sel: &pg_query::protobuf::SelectStmt,
    expr: &pg_query::protobuf::Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    let scope = Scope::new(&sel.from_clause, &[], lookup, None);
    operand_type(expr, &scope).filter(|t| !t.is_empty() && t != "unknown")
}

/// The type of an outer reference (`col`, or `q.col` with any schema in
/// front) read from one JOIN in an enclosing query's FROM, as PostgreSQL's
/// join RTE types it (`transformFromClauseItem`): a leaf's own column, a
/// USING / NATURAL join's merged column (the sides' `select_common_type`),
/// a column of the join's alias or of a USING alias. The CTEs in view
/// (`VISIBLE_CTES`) name the leaves that are CTEs.
///
/// `None` when the join does not answer to the reference -- a qualifier
/// naming none of its entries, a name none of its columns has -- so the
/// caller looks on; `Some(None)` when it does but this cannot type it
/// (untyped, as before).
pub(crate) fn join_column_type(
    item: &pg_query::protobuf::Node,
    parts: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<Option<String>> {
    let visible = crate::VISIBLE_CTES.with(|v| v.borrow().clone());
    let ctes: Vec<Cte> = visible
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(c),
            _ => None,
        })
        .map(|c| {
            let query = match c.ctequery.as_deref().and_then(|n| n.node.as_ref()) {
                Some(N::SelectStmt(q)) => Some(((**q).clone(), c.aliascolnames.clone())),
                _ => None,
            };
            (c.ctename.clone(), query)
        })
        .collect();
    let scope = Scope::new(std::slice::from_ref(item), &ctes, lookup, None);
    let field = |s: &str| pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String {
            sval: s.to_string(),
        })),
    };
    let (qualifier, col) = match parts {
        [.., q, c] => (Some(q), c),
        [c] => (None, c),
        _ => return None,
    };
    let reference = pg_query::protobuf::ColumnRef {
        fields: qualifier
            .iter()
            .map(|q| field(q))
            .chain([field(col)])
            .collect(),
        location: -1,
    };
    match qualifier {
        Some(q) => {
            let answers = scope.tables.iter().any(|(a, _)| a == q)
                || scope.using_aliases.iter().any(|(a, _)| a == q);
            if answers {
                Some(scope.column_type(&reference))
            } else if !scope.complete {
                // A leaf this cannot see into may be the one named.
                Some(None)
            } else {
                None
            }
        }
        None => match scope.column_type(&reference) {
            Some(t) => Some(Some(t)),
            None if scope.complete && scope.knows(col) == Some(false) => None,
            // Named more than once, or possibly by a leaf this cannot see
            // into: not typed here.
            None => Some(None),
        },
    }
}

/// The type an operand has statically, or `None`.
/// An operand's type, a domain read as its base type (operator resolution
/// looks through a domain, `getBaseType`): a view column of
/// `information_schema.sql_identifier` compares as `name`.
fn operand_type(n: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    let t = operand_type_declared(n, scope)?;
    Some(crate::domains::domain_base(&t).unwrap_or(t))
}

fn operand_type_declared(n: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    match n.node.as_ref()? {
        N::AConst(c) if !c.isnull => match c.val.as_ref()? {
            Val::Ival(_) => Some("int4".into()),
            // An integer literal past int4 is a bigint (`5000000000`), one
            // past int8 a numeric -- pg_query hands both over as a float.
            Val::Fval(f) if f.fval.bytes().all(|b| b.is_ascii_digit()) => Some(
                if f.fval.parse::<i64>().is_ok() {
                    "int8"
                } else {
                    "numeric"
                }
                .into(),
            ),
            Val::Fval(_) => Some("numeric".into()),
            Val::Boolval(_) => Some("bool".into()),
            _ => None,
        },
        N::TypeCast(tc) => {
            let t = type_name_of(tc.type_name.as_ref()?);
            (!t.ends_with("[]")).then_some(t)
        }
        N::ColumnRef(c) => scope.column_type(c),
        // `current_date` and friends: their fixed types.
        N::SqlvalueFunction(f) => {
            use pg_query::protobuf::SqlValueFunctionOp as O;
            Some(
                match O::try_from(f.op).ok()? {
                    O::SvfopCurrentDate => "date",
                    O::SvfopCurrentTime | O::SvfopCurrentTimeN => "timetz",
                    O::SvfopCurrentTimestamp | O::SvfopCurrentTimestampN => "timestamptz",
                    O::SvfopLocaltime | O::SvfopLocaltimeN => "time",
                    O::SvfopLocaltimestamp | O::SvfopLocaltimestampN => "timestamp",
                    _ => return None,
                }
                .into(),
            )
        }
        // `count(*)` is bigint.
        N::FuncCall(f) if f.agg_star && f.over.is_none() => {
            (func_name(f)?.as_str() == "count").then(|| "int8".into())
        }
        // `->>` / `#>>` extract text; `->` / `#>` keep the json kind.
        N::AExpr(e)
            if pg_query::protobuf::AExprKind::try_from(e.kind)
                == Ok(pg_query::protobuf::AExprKind::AexprOp)
                && matches!(op_of(e).as_deref(), Some("->" | "->>" | "#>" | "#>>")) =>
        {
            let l = operand_type(e.lexpr.as_deref()?, scope)?;
            if !matches!(l.as_str(), "json" | "jsonb") {
                return None;
            }
            Some(if op_of(e)?.ends_with(">>") {
                "text".into()
            } else {
                l
            })
        }
        N::ParamRef(p) => declared_param_type(usize::try_from(p.number).ok()?),
        // A scalar subquery: its one column's type, in a scope of its own.
        N::SubLink(sl)
            if pg_query::protobuf::SubLinkType::try_from(sl.sub_link_type)
                == Ok(pg_query::protobuf::SubLinkType::ExprSublink) =>
        {
            let Some(N::SelectStmt(sub)) = sl.subselect.as_deref().and_then(|n| n.node.as_ref())
            else {
                return None;
            };
            if sub.with_clause.is_some() {
                return None;
            }
            first_output_type(sub, scope)?.or_else(|| Some("text".into()))
        }
        // CASE / COALESCE / GREATEST / LEAST: their branches' common type,
        // untyped literals taking it.
        N::CaseExpr(c) => {
            let mut parts: Vec<&pg_query::protobuf::Node> = c
                .args
                .iter()
                .filter_map(|w| match w.node.as_ref() {
                    Some(N::CaseWhen(cw)) => cw.result.as_deref(),
                    _ => None,
                })
                .collect();
            parts.extend(c.defresult.as_deref());
            common_type(&parts, scope)
        }
        N::CoalesceExpr(c) => common_type(&c.args.iter().collect::<Vec<_>>(), scope),
        N::MinMaxExpr(m) => common_type(&m.args.iter().collect::<Vec<_>>(), scope),
        N::AExpr(e)
            if pg_query::protobuf::AExprKind::try_from(e.kind)
                == Ok(pg_query::protobuf::AExprKind::AexprNullif) =>
        {
            operand_type(e.lexpr.as_deref()?, scope)
        }
        // A built-in comparison, pattern match, IN, BETWEEN or IS [NOT]
        // DISTINCT FROM is boolean, as are AND / OR / NOT and IS [NOT] NULL.
        N::AExpr(e)
            if op_of(e).is_some_and(|op| {
                !user_ops::defines(&op)
                    && match pg_query::protobuf::AExprKind::try_from(e.kind) {
                        Ok(pg_query::protobuf::AExprKind::AexprOp) => matches!(
                            op.as_str(),
                            "=" | "<>"
                                | "!="
                                | "<"
                                | "<="
                                | ">"
                                | ">="
                                | "~~"
                                | "!~~"
                                | "~~*"
                                | "!~~*"
                        ),
                        Ok(
                            pg_query::protobuf::AExprKind::AexprDistinct
                            | pg_query::protobuf::AExprKind::AexprNotDistinct
                            | pg_query::protobuf::AExprKind::AexprLike
                            | pg_query::protobuf::AExprKind::AexprIlike
                            | pg_query::protobuf::AExprKind::AexprIn
                            | pg_query::protobuf::AExprKind::AexprBetween
                            | pg_query::protobuf::AExprKind::AexprNotBetween,
                        ) => true,
                        _ => false,
                    }
            }) =>
        {
            Some("bool".into())
        }
        N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_) => Some("bool".into()),
        // `||` over a string is text; date arithmetic keeps its kind.
        N::AExpr(e)
            if pg_query::protobuf::AExprKind::try_from(e.kind)
                == Ok(pg_query::protobuf::AExprKind::AexprOp)
                && matches!(op_of(e).as_deref(), Some("||" | "+" | "-")) =>
        {
            let op = op_of(e)?;
            let l = e.lexpr.as_deref().and_then(|n| operand_type(n, scope));
            let r = e.rexpr.as_deref().and_then(|n| operand_type(n, scope));
            let stringy = |t: &Option<String>| {
                t.as_deref()
                    .is_some_and(|t| matches!(t, "text" | "varchar" | "bpchar" | "name"))
            };
            if op == "||" {
                return (stringy(&l) || stringy(&r)).then(|| "text".to_string());
            }
            match (l.as_deref(), r.as_deref(), op.as_str()) {
                (Some("date"), Some("int4" | "int2"), "+" | "-")
                | (Some("int4" | "int2"), Some("date"), "+") => Some("date".into()),
                (Some("date"), Some("date"), "-") => Some("int4".into()),
                // Numbers meet as numbers (the arithmetic rule below).
                (Some(l), Some(r), _)
                    if category(l) == Some("numeric") && category(r) == Some("numeric") =>
                {
                    Some(numeric_meet(l, r))
                }
                _ => None,
            }
        }
        // A window function's result: the ranking functions are bigint (ntile
        // integer, the distributions float8), the value functions their
        // argument's type, an aggregate over a window its overload's result.
        N::FuncCall(f) if f.over.is_some() => {
            let name = func_name(f)?;
            match name.as_str() {
                "row_number" | "rank" | "dense_rank" => Some("int8".into()),
                "count" => Some("int8".into()),
                "ntile" => Some("int4".into()),
                "percent_rank" | "cume_dist" => Some("float8".into()),
                "lag" | "lead" | "first_value" | "last_value" | "nth_value" => {
                    operand_type(f.args.first()?, scope)
                }
                _ => {
                    let args = f
                        .args
                        .iter()
                        .map(|a| operand_type(a, scope))
                        .collect::<Option<Vec<_>>>()?;
                    crate::funcsig::result_type(&name, &args)
                }
            }
        }
        // A composite column's field, `(c).b`: the field's declared type.
        N::AIndirection(ind)
            if matches!(
                ind.indirection.as_slice(),
                [one] if matches!(one.node.as_ref(), Some(N::String(_)))
            ) =>
        {
            let Some(N::String(field)) = ind.indirection[0].node.as_ref() else {
                return None;
            };
            let base = match ind.arg.as_deref()?.node.as_ref()? {
                N::ColumnRef(c) => scope.column_type(c)?,
                _ => return None,
            };
            let (_, fields) = user_composite(&base)?;
            fields
                .into_iter()
                .find(|(n, _)| *n == field.sval)
                .map(|(_, t)| t)
        }
        // An array subscript: the ELEMENT type; a slice keeps the array's.
        N::AIndirection(ind)
            if !ind.indirection.is_empty()
                && ind
                    .indirection
                    .iter()
                    .all(|i| matches!(i.node.as_ref(), Some(N::AIndices(_)))) =>
        {
            let base = match ind.arg.as_deref()?.node.as_ref()? {
                N::ColumnRef(c) => scope.column_type(c)?,
                _ => return None,
            };
            let slice = ind
                .indirection
                .iter()
                .any(|i| matches!(i.node.as_ref(), Some(N::AIndices(x)) if x.is_slice));
            let element = base.strip_suffix("[]")?.to_string();
            (!slice).then_some(element)
        }
        // A built-in's result, as the overload its arguments select returns.
        N::FuncCall(f) if f.over.is_none() && !f.agg_star && f.agg_order.is_empty() => {
            let name = func_name(f)?;
            if correlated::user_function_named(&name) || f.funcname.len() > 2 {
                return None;
            }
            let args: Vec<String> = f
                .args
                .iter()
                .map(|a| match a.node.as_ref() {
                    Some(N::AConst(c))
                        if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
                    {
                        Some(String::new())
                    }
                    _ => operand_type(a, scope),
                })
                .collect::<Option<Vec<_>>>()?;
            crate::funcsig::result_type(&name, &args)
        }
        // Arithmetic over two numbers is a number: `a + 0` is numeric, so
        // `a + 0 = 'x'::text` is judged like `a = 'x'::text`.
        N::AExpr(e)
            if pg_query::protobuf::AExprKind::try_from(e.kind)
                == Ok(pg_query::protobuf::AExprKind::AexprOp)
                && matches!(op_of(e).as_deref(), Some("+" | "-" | "*" | "/" | "%")) =>
        {
            let l = operand_type(e.lexpr.as_deref()?, scope)?;
            let r = operand_type(e.rexpr.as_deref()?, scope)?;
            (category(&l) == Some("numeric") && category(&r) == Some("numeric"))
                .then(|| numeric_meet(&l, &r))
        }
        _ => None,
    }
}

fn op_of(e: &pg_query::protobuf::AExpr) -> Option<String> {
    match e.name.last()?.node.as_ref()? {
        N::String(s) => Some(s.sval.clone()),
        _ => None,
    }
}

/// The type two numbers meet as in arithmetic: a float wins (`int + float8`
/// is float8), then numeric, then the wider integer.
fn numeric_meet(l: &str, r: &str) -> String {
    if l == r {
        return l.to_string();
    }
    let has = |t: &str| l == t || r == t;
    if has("float8") || has("float4") {
        "float8".into()
    } else if has("numeric") {
        "numeric".into()
    } else if has("int8") {
        "int8".into()
    } else {
        "int4".into()
    }
}

fn mismatch(op: &str, l: &str, r: &str, location: i32) -> Error {
    set_error_location(location);
    Error::UndefinedFunction(format!(
        "operator does not exist: {} {op} {}",
        display_type(l),
        display_type(r)
    ))
}

/// The element type of the array operand of `x op ANY / ALL (r)`, every
/// dimension's `[]` dropped: an array type carries no dimension count, so
/// `ARRAY[b]` over an `int[]` `b` and `'{{1}}'::int[]` both hold `int4`
/// elements (`make_scalar_array_op` takes `get_base_element_type`). An
/// ARRAY of untyped literals alone is a `text[]`; `None` when not typed.
fn any_element_type(r: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    let base = |t: &str| {
        let t = t.trim_end_matches("[]").to_string();
        crate::domains::domain_base(&t).unwrap_or(t)
    };
    match r.node.as_ref()? {
        N::AArrayExpr(a) => {
            let mut literal = false;
            for el in &a.elements {
                let t = match el.node.as_ref() {
                    Some(N::AConst(c)) if matches!(c.val, Some(Val::Sval(_))) => {
                        literal = true;
                        None
                    }
                    Some(N::TypeCast(tc)) => tc.type_name.as_ref().map(type_name_of),
                    Some(N::AArrayExpr(_)) => any_element_type(el, scope),
                    _ => operand_type(el, scope),
                };
                if let Some(t) = t {
                    return Some(base(&t));
                }
            }
            literal.then(|| "text".to_string())
        }
        N::TypeCast(tc) => {
            let t = type_name_of(tc.type_name.as_ref()?);
            t.ends_with("[]").then(|| base(&t))
        }
        _ => operand_type(r, scope).and_then(|t| t.ends_with("[]").then(|| base(&t))),
    }
}

/// The operators `pg_operator` has over `(record, record)` in PostgreSQL
/// 15: `record_eq` .. `record_ge` and their `record_image_*` forms. (`!=`
/// is the parser's spelling of `<>`.)
fn is_record_operator(op: &str) -> bool {
    matches!(
        op,
        "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=" | "*=" | "*<>" | "*<" | "*<=" | "*>" | "*>="
    )
}

/// Check one `AExpr`: both sides typed, and no operator between them.
fn check_aexpr(e: &pg_query::protobuf::AExpr, scope: &Scope) -> Result<()> {
    use pg_query::protobuf::AExprKind as K;
    let Some(op) = op_of(e) else {
        return Ok(());
    };
    if user_ops::defines(&op) {
        return Ok(());
    }
    let kind = K::try_from(e.kind).ok();
    // An enum compared with an untyped literal reads the literal as the enum
    // when the statement is analysed (`c = 'zz'` is 22P02 at the literal on
    // 15.19, even over an empty table).
    if matches!(
        kind,
        Some(K::AexprOp | K::AexprDistinct | K::AexprNotDistinct)
    ) {
        for (col, lit) in [
            (e.lexpr.as_deref(), e.rexpr.as_deref()),
            (e.rexpr.as_deref(), e.lexpr.as_deref()),
        ] {
            let (Some(col), Some(lit)) = (col, lit) else {
                continue;
            };
            let Some(N::AConst(c)) = lit.node.as_ref() else {
                continue;
            };
            let Some(pg_query::protobuf::a_const::Val::Sval(label)) = c.val.as_ref() else {
                continue;
            };
            if let Some(ty) = operand_type(col, scope).filter(|t| crate::user_enum(t).is_some()) {
                crate::cast_value(Bson::String(label.sval.clone()), &ty)?;
            }
        }
        // An enum has its comparison operators against itself only: beside
        // any other TYPED operand (`c = d::text`) there is none (42883).
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            // A composite is installed among the user types with no labels
            // (`set_user_types`), so `user_enum` answers for it too: it is
            // not an enum, and two composites are `record`'s (RP-590).
            let is_enum = |t: &str| crate::user_enum(t).is_some() && user_composite(t).is_none();
            if (is_enum(&l) || is_enum(&r))
                && l != r
                && matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=")
            {
                let op = if op == "!=" { "<>" } else { op.as_str() };
                return Err(mismatch(op, &l, &r, e.location));
            }
        }
    }
    // `c IN ('a', 'zz')` over an enum: each untyped literal is read as the
    // enum when the statement is analysed.
    if kind == Some(K::AexprIn) {
        if let (Some(col), Some(N::List(items))) = (
            e.lexpr.as_deref(),
            e.rexpr.as_deref().and_then(|n| n.node.as_ref()),
        ) {
            if let Some(ty) = operand_type(col, scope).filter(|t| crate::user_enum(t).is_some()) {
                for (i, item) in items.items.iter().enumerate() {
                    // A cancellation point every so often, as in the typing
                    // loop below (RP-748).
                    if i % 1024 == 1023 {
                        crate::cancel::check()?;
                    }
                    if let Some(N::AConst(c)) = item.node.as_ref() {
                        if let Some(pg_query::protobuf::a_const::Val::Sval(label)) = c.val.as_ref()
                        {
                            crate::cast_value(Bson::String(label.sval.clone()), &ty)?;
                        }
                    }
                }
            }
        }
    }
    // A ROW beside a typed operand that is not one: no operator (RP-530).
    if matches!(
        kind,
        Some(K::AexprOp | K::AexprDistinct | K::AexprNotDistinct | K::AexprNullif)
    ) {
        if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
            // `ROW(...) op (SELECT ...)` is a row comparison SubLink, not an
            // operator (`transformAExprOp`): its subquery's width is checked
            // against the row (`(1, 2) = (select 1)` is 42601).
            let row_compare = kind == Some(K::AexprOp)
                && matches!(l.node.as_ref(), Some(N::RowExpr(_)))
                && matches!(r.node.as_ref(), Some(N::SubLink(_)));
            if !row_compare {
                row_against_non_row(&op, l, r, e.location, scope)?;
            }
        }
    }
    if kind == Some(K::AexprIn) {
        if let (Some(l), Some(N::List(items))) = (
            e.lexpr.as_deref(),
            e.rexpr.as_deref().and_then(|n| n.node.as_ref()),
        ) {
            // `transformAExprIn` falls back to one `=` (`<>` for NOT IN)
            // per item, in the list's order.
            for (i, item) in items.items.iter().enumerate() {
                // A cancellation point every so often: over a 200,000-item
                // list this loop was the longest stretch a cancel could not
                // reach, about 70 ms in a debug build and half a second
                // under load, past RP-202's budget (RP-748).
                if i % 1024 == 1023 {
                    crate::cancel::check()?;
                }
                row_against_non_row(&op, l, item, e.location, scope)?;
            }
        }
    }
    // `fs <> ALL (ARRAY[1])` and `fs = ANY (int_array)`: the row against
    // the array's element type (`make_scalar_array_op`).
    if matches!(kind, Some(K::AexprOpAny | K::AexprOpAll)) {
        if let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) {
            if let Some(row) = row_operand_type(l, scope) {
                let element = match r.node.as_ref() {
                    Some(N::AArrayExpr(a)) if !a.elements.is_empty() => {
                        common_type(&a.elements.iter().collect::<Vec<_>>(), scope)
                    }
                    // A cast to an array type (`'{1}'::int[]`), which
                    // `operand_type` leaves untyped, or a typed array.
                    Some(N::TypeCast(tc)) => tc
                        .type_name
                        .as_ref()
                        .map(type_name_of)
                        .and_then(|t| t.strip_suffix("[]").map(str::to_string)),
                    _ => operand_type_declared(r, scope)
                        .and_then(|t| t.strip_suffix("[]").map(str::to_string)),
                };
                if let Some(t) = element.filter(|t| t != "record" && user_composite(t).is_none()) {
                    let op = if op == "!=" { "<>" } else { op.as_str() };
                    return Err(mismatch(op, &row, &t, e.location));
                }
            }
        }
    }
    // `fs BETWEEN 1 AND 2` is `fs >= 1 AND fs <= 2` (NOT: `<` and `>`).
    if matches!(kind, Some(K::AexprBetween | K::AexprNotBetween)) {
        if let (Some(l), Some(N::List(bounds))) = (
            e.lexpr.as_deref(),
            e.rexpr.as_deref().and_then(|n| n.node.as_ref()),
        ) {
            let ops = if kind == Some(K::AexprBetween) {
                [">=", "<="]
            } else {
                ["<", ">"]
            };
            for (b, op) in bounds.items.iter().zip(ops) {
                row_against_non_row(op, l, b, e.location, scope)?;
            }
        }
    }
    // A COMPOSITE has only the record operators. Those take `record` on
    // both sides, which every composite coerces to, so two DIFFERENT
    // composite types still compare (`ct = ct2` is `record_eq`, RP-590),
    // and `record_eq` / `record_cmp` check the field types they reach when
    // the statement runs (`record_compare`). Any other operator between two
    // composite types has no candidate (`ct + ct2`), and a user cast never
    // supplies one. A composite beside a value that is not a row is
    // `row_against_non_row`'s.
    if kind == Some(K::AexprOp) && !is_record_operator(&op) {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            let composite = |t: &str| user_composite(t).is_some();
            if (composite(&l) && composite(&r))
                && user_casts::type_oid(&l) != user_casts::type_oid(&r)
                && l != "record"
                && r != "record"
            {
                let op = if op == "!=" { "<>" } else { op.as_str() };
                return Err(mismatch(op, &l, &r, e.location));
            }
        }
    }
    // Any binary operator over two KNOWN built-in types is resolved against
    // `pg_operator`, as parse analysis resolves it: no candidate that takes
    // the two types (exactly, by an implicit cast, or polymorphically) is
    // 42883 at the operator, where lowering it compared the carried values
    // (`int2 = xid` answered true, `numeric = oid` false) or failed at run
    // time with a 0A000 "comparing ... is not supported yet".
    if kind == Some(K::AexprOp) && e.lexpr.is_some() {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            let name = if op == "!=" { "<>" } else { op.as_str() };
            if crate::funcsig::operator_resolves(name, &l, &r) == Some(false) {
                return Err(mismatch(name, &l, &r, e.location));
            }
            if crate::funcsig::operator_ambiguous(name, &l, &r) {
                set_error_location(e.location);
                return Err(Error::Sqlstate(
                    "42725",
                    format!(
                        "operator is not unique: {} {name} {}",
                        display_type(&l),
                        display_type(&r)
                    ),
                ));
            }
        }
    }
    // NULLIF and IS [NOT] DISTINCT FROM resolve `a = b` the same way; an
    // untyped literal side is read as the other side's type, but named
    // `unknown` when there is no `=` for it (`nullif(point, '(1,2)')`).
    if matches!(
        kind,
        Some(K::AexprNullif | K::AexprDistinct | K::AexprNotDistinct)
    ) {
        let untyped = |n: Option<&pg_query::protobuf::Node>| {
            matches!(n.and_then(|n| n.node.as_ref()), Some(N::AConst(c))
                if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))))
        };
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        let (ln, rn) = (e.lexpr.as_deref(), e.rexpr.as_deref());
        let (l, r) = (side(ln), side(rn));
        let refused = match (&l, &r) {
            (Some(l), Some(r)) => crate::funcsig::operator_resolves("=", l, r) == Some(false),
            (Some(t), None) if untyped(rn) => {
                crate::funcsig::operator_resolves("=", t, t) == Some(false)
            }
            (None, Some(t)) if untyped(ln) => {
                crate::funcsig::operator_resolves("=", t, t) == Some(false)
            }
            _ => false,
        };
        if refused {
            let name = |t: &Option<String>| t.clone().unwrap_or_else(|| "unknown".into());
            return Err(mismatch("=", &name(&l), &name(&r), e.location));
        }
    }
    // `oid` has comparison operators and nothing arithmetic: `oid + integer`,
    // `oid - oid` and a prefix `- oid` are all 42883 on PostgreSQL, where
    // evaluating them as the integers the values are carried as answered.
    if kind == Some(K::AexprOp) && matches!(op.as_str(), "+" | "-" | "*" | "/" | "%" | "^") {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        let (l, r) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref()));
        if l.as_deref() == Some("oid") || r.as_deref() == Some("oid") {
            match (e.lexpr.is_some(), l, r) {
                (false, _, Some(r)) => {
                    set_error_location(e.location);
                    return Err(Error::UndefinedFunction(format!(
                        "operator does not exist: {op} {}",
                        display_type(&r)
                    )));
                }
                (true, Some(l), Some(r)) => return Err(mismatch(&op, &l, &r, e.location)),
                _ => {}
            }
        }
    }
    // `tid || x` is `anynonarray || text`: the other side must be a string
    // (or untyped). `tid || integer` has no operator.
    if kind == Some(K::AexprOp) && op == "||" {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            let other = if l == "tid" {
                Some(&r)
            } else if r == "tid" {
                Some(&l)
            } else {
                None
            };
            if other.is_some_and(|t| t != "unknown" && category(t) != Some("string")) {
                return Err(mismatch(&op, &l, &r, e.location));
            }
        }
    }
    // `tid` has the six comparisons against another tid and nothing else
    // (`pg_operator`): `tid = integer` and `tid + 1` are 42883, where
    // comparing or adding the numbers a tid is carried as answered.
    if kind == Some(K::AexprOp) && op != "||" {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        let (l, r) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref()));
        let tid = |t: &Option<String>| t.as_deref() == Some("tid");
        if tid(&l) || tid(&r) {
            let comparison = matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=");
            // Beside a tid an untyped literal is a tid for a comparison and
            // has no operator to resolve to otherwise (`tid + '1'`).
            let untyped = |n: Option<&pg_query::protobuf::Node>| {
                matches!(n.and_then(|n| n.node.as_ref()), Some(N::AConst(c))
                    if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))))
            };
            let (l, r) = if comparison {
                (l, r)
            } else {
                let fill =
                    |t: Option<String>, n| t.or_else(|| untyped(n).then(|| "unknown".to_string()));
                (fill(l, e.lexpr.as_deref()), fill(r, e.rexpr.as_deref()))
            };
            let typed = |t: &Option<String>| {
                t.as_deref().is_some_and(|t| t != "unknown") || !comparison && t.is_some()
            };
            match (e.lexpr.is_some(), &l, &r) {
                (false, _, Some(r)) => {
                    set_error_location(e.location);
                    return Err(Error::UndefinedFunction(format!(
                        "operator does not exist: {op} {}",
                        display_type(r)
                    )));
                }
                (true, Some(lt), Some(rt))
                    if typed(&l) && typed(&r) && (!comparison || lt != rt) =>
                {
                    let op = if op == "!=" { "<>" } else { op.as_str() };
                    return Err(mismatch(op, lt, rt, e.location));
                }
                _ => {}
            }
        }
    }
    // A prefix `-` / `+` exists for the numbers and `interval` only:
    // `- text` is 42883, where the lowering answered it as `0 - text`.
    if kind == Some(K::AexprOp) && e.lexpr.is_none() && matches!(op.as_str(), "+" | "-") {
        if let Some(r) = e.rexpr.as_deref().and_then(|n| operand_type(n, scope)) {
            if matches!(
                category(&r),
                Some("string" | "bool" | "datetime" | "bytea" | "uuid" | "jsonb")
            ) || matches!(r.as_str(), "json")
            {
                set_error_location(e.location);
                return Err(Error::UndefinedFunction(format!(
                    "operator does not exist: {op} {}",
                    display_type(&r)
                )));
            }
        }
    }
    // `boolean` has no arithmetic at all: `integer + boolean` is 42883.
    if kind == Some(K::AexprOp) && matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            // Nor do the string types (`text + integer`, `text + text`).
            // Judged only between BUILT-IN categories: an extension or user
            // type can define one (`hstore - text` exists).
            let string = |t: &str| category(t) == Some("string");
            let known = category(&l).is_some() && category(&r).is_some();
            // `jsonb - text` is a key's delete, checked below.
            let jsonb_delete = op == "-" && l == "jsonb";
            if known && !jsonb_delete && (l == "bool" || r == "bool" || string(&l) || string(&r)) {
                return Err(mismatch(&op, &l, &r, e.location));
            }
        }
    }
    // `json` / `jsonb` have no arithmetic: `jsonb + integer` is 42883 where
    // evaluating it over the value's text answered `text + integer`. The one
    // operator among them is `jsonb - text | integer | text[]` (delete).
    if kind == Some(K::AexprOp) && matches!(op.as_str(), "+" | "-" | "*" | "/" | "%") {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            let json = |t: &str| matches!(t, "json" | "jsonb");
            let delete = op == "-"
                && l == "jsonb"
                && matches!(r.as_str(), "text" | "int4" | "int2" | "int8" | "text[]");
            // The other side a built-in type: an extension may define one.
            let known = |t: &str| json(t) || category(t).is_some() || t == "text[]";
            if (json(&l) || json(&r)) && known(&l) && known(&r) && !delete {
                return Err(mismatch(&op, &l, &r, e.location));
            }
        }
    }
    // Two row constructors compare field by field: each pair needs an
    // operator of its own (`row(id, t) = row(true, 'a')` is integer =
    // boolean).
    if kind == Some(K::AexprOp)
        && matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=")
    {
        if let (Some(N::RowExpr(l)), Some(N::RowExpr(r))) = (
            e.lexpr.as_deref().and_then(|n| n.node.as_ref()),
            e.rexpr.as_deref().and_then(|n| n.node.as_ref()),
        ) {
            if l.args.len() == r.args.len() {
                for (a, b) in l.args.iter().zip(&r.args) {
                    let (Some(at), Some(bt)) = (operand_type(a, scope), operand_type(b, scope))
                    else {
                        continue;
                    };
                    if matches!((category(&at), category(&bt)), (Some(x), Some(y)) if x != y) {
                        let op = if op == "!=" { "<>" } else { op.as_str() };
                        return Err(mismatch(op, &at, &bt, e.location));
                    }
                }
            }
            return Ok(());
        }
    }
    let Some(l) = e.lexpr.as_deref().and_then(|n| operand_type(n, scope)) else {
        return Ok(());
    };
    // An array beside a scalar of a built-in category: no comparison
    // operator takes them (`integer[] = integer`), where the lowering
    // matched an array containing the value.
    if kind == Some(K::AexprOp)
        && matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=")
    {
        if let Some(r) = e.rexpr.as_deref().and_then(|n| operand_type(n, scope)) {
            let array_beside_scalar = |a: &str, b: &str| {
                a.strip_suffix("[]")
                    .is_some_and(|el| category(el).is_some())
                    && category(b).is_some()
            };
            if array_beside_scalar(&l, &r) || array_beside_scalar(&r, &l) {
                let op = if op == "!=" { "<>" } else { op.as_str() };
                return Err(mismatch(op, &l, &r, e.location));
            }
        }
    }
    // `x op ANY / ALL (array)` compares `x` with the array's ELEMENT type
    // (`make_scalar_array_op`), and an array's element type is never an
    // array: `ARRAY['{x,y}'::text[]]` and `'{{x,y}}'::text[]` are both a
    // `text[]` of two dimensions. So an array `x` meets a scalar, which no
    // comparison operator takes (`text[] = text`, 42883), where the
    // evaluation compared it with each sub-array and answered rows (RP-740).
    if matches!(kind, Some(K::AexprOpAny | K::AexprOpAll))
        && matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=")
        && l.strip_suffix("[]")
            .is_some_and(|el| category(el).is_some())
    {
        if let Some(r) = e.rexpr.as_deref() {
            // An untyped literal is read as `x`'s array type, and an array
            // type has none of its own (42704, parse_oper.c).
            if matches!(r.node.as_ref(), Some(N::AConst(c))
                if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))))
            {
                set_error_location(e.location);
                return Err(Error::UndefinedObject(format!(
                    "could not find array type for data type {}",
                    display_type(&l)
                )));
            }
            if let Some(el) = any_element_type(r, scope) {
                if category(&el).is_some() {
                    let op = if op == "!=" { "<>" } else { op.as_str() };
                    return Err(mismatch(op, &l, &el, e.location));
                }
            }
        }
    }
    let Some(lc) = category(&l) else {
        return Ok(());
    };
    // `x op ANY / ALL (ARRAY[...])`: each element against `x`, and a typed
    // array's element type.
    if matches!(kind, Some(K::AexprOpAny | K::AexprOpAll)) {
        let elements: Vec<String> = match e.rexpr.as_deref().and_then(|n| n.node.as_ref()) {
            Some(N::AArrayExpr(a)) => {
                let typed: Vec<String> = a
                    .elements
                    .iter()
                    .filter_map(|el| operand_type(el, scope))
                    .collect();
                // Untyped literals alone make a text[] (`ARRAY['x']`).
                let literal = a.elements.iter().any(|el| {
                    matches!(el.node.as_ref(), Some(N::AConst(c))
                        if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))))
                });
                if typed.is_empty() && literal {
                    vec!["text".to_string()]
                } else {
                    typed
                }
            }
            _ => e
                .rexpr
                .as_deref()
                .and_then(|n| operand_type(n, scope))
                .and_then(|t| t.strip_suffix("[]").map(str::to_string))
                .into_iter()
                .collect(),
        };
        // A nested array's elements (`1 = any (array['{x}'::text[]])` is
        // `integer = text`), which `operand_type` leaves untyped.
        let elements = if elements.is_empty() {
            e.rexpr
                .as_deref()
                .and_then(|r| any_element_type(r, scope))
                .into_iter()
                .collect()
        } else {
            elements
        };
        if let Some(r) = elements
            .iter()
            .find(|r| category(r).is_some_and(|rc| rc != lc))
        {
            if matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
                let op = if op == "!=" { "<>" } else { op.as_str() };
                return Err(mismatch(op, &l, r, e.location));
            }
        }
        return Ok(());
    }
    // `x BETWEEN lo AND hi` is `x >= lo AND x <= hi` (NOT: `x < lo OR x >
    // hi`): each bound needs that operator.
    if matches!(kind, Some(K::AexprBetween | K::AexprNotBetween)) {
        if let Some(N::List(bounds)) = e.rexpr.as_deref().and_then(|n| n.node.as_ref()) {
            let ops = if kind == Some(K::AexprBetween) {
                [">=", "<="]
            } else {
                ["<", ">"]
            };
            for (b, op) in bounds.items.iter().zip(ops) {
                if let Some(r) = operand_type(b, scope) {
                    if category(&r).is_some_and(|rc| rc != lc) {
                        return Err(mismatch(op, &l, &r, e.location));
                    }
                }
            }
        }
        return Ok(());
    }
    match kind {
        // `NULLIF(a, b)` and `a IS [NOT] DISTINCT FROM b` both resolve `a = b`.
        Some(K::AexprNullif | K::AexprDistinct | K::AexprNotDistinct) => {
            let Some(r) = e.rexpr.as_deref().and_then(|n| operand_type(n, scope)) else {
                return Ok(());
            };
            if category(&r).is_some_and(|rc| rc != lc) {
                return Err(mismatch("=", &l, &r, e.location));
            }
        }
        Some(K::AexprOp) if matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") => {
            // An untyped literal takes the other side's type when the
            // statement is analysed: one that is not valid input fails then.
            if let Some(r) = e.rexpr.as_deref() {
                literal_fits(r, &l)?;
            }
            let Some(r) = e.rexpr.as_deref().and_then(|n| operand_type(n, scope)) else {
                return Ok(());
            };
            if category(&r).is_some_and(|rc| rc != lc) {
                let op = if op == "!=" { "<>" } else { op.as_str() };
                return Err(mismatch(op, &l, &r, e.location));
            }
        }
        Some(K::AexprIn) => {
            let Some(N::List(items)) = e.rexpr.as_deref().and_then(|n| n.node.as_ref()) else {
                return Ok(());
            };
            for (i, item) in items.items.iter().enumerate() {
                // A cancellation point every so often: a 200,000-item list
                // is typed here for a fifth of a second (RP-202).
                if i % 1024 == 1023 {
                    crate::cancel::check()?;
                }
                if let Some(r) = operand_type(item, scope) {
                    if category(&r).is_some_and(|rc| rc != lc) {
                        return Err(mismatch(&op, &l, &r, e.location));
                    }
                }
            }
            // Each untyped member is read as a type when the statement is
            // analysed, so one that is not valid input for it fails before
            // any row is compared, and over no rows (RP-806). More than one
            // member with no Var is coerced to their common type with the
            // left operand's (`transformAExprIn`); otherwise each member is
            // an `=` of its own, where the untyped side takes the left's
            // type. A common type this cannot tell checks nothing.
            let row_from = crate::correlated::row_params_from();
            let mut nonvars: Vec<&pg_query::protobuf::Node> = Vec::new();
            for (i, item) in items.items.iter().enumerate() {
                if i % 1024 == 1023 {
                    crate::cancel::check()?;
                }
                if !crate::in_member_has_var(item, row_from) {
                    nonvars.push(item);
                }
            }
            let target = match (e.lexpr.as_deref(), nonvars.len()) {
                (Some(lexpr), n) if n > 1 => {
                    let mut parts = Vec::with_capacity(n + 1);
                    parts.push(lexpr);
                    parts.extend(nonvars.iter().copied());
                    common_type(&parts, scope).map(crate::domain_base)
                }
                _ => Some(crate::domain_base(l.clone())),
            };
            if let Some(target) = target {
                for (i, item) in nonvars.iter().enumerate() {
                    if i % 1024 == 1023 {
                        crate::cancel::check()?;
                    }
                    literal_fits(item, &target)?;
                }
            }
        }
        Some(K::AexprLike | K::AexprIlike) | Some(K::AexprOp)
            if matches!(op.as_str(), "~~" | "~~*" | "!~~" | "!~~*") =>
        {
            // An untyped string literal is `unknown`, which resolves to the
            // other side's type only if that type has a `~~`: only the
            // string types and bytea do (`integer ~~ unknown` is 42883, and
            // the per-row evaluator answered NULL for it).
            let untyped = matches!(
                e.rexpr.as_deref().and_then(|n| n.node.as_ref()),
                Some(N::AConst(c)) if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_)))
            );
            if untyped && lc != "string" && lc != "bytea" {
                return Err(mismatch(&op, &l, "unknown", e.location));
            }
            let Some(r) = e.rexpr.as_deref().and_then(|n| operand_type(n, scope)) else {
                return Ok(());
            };
            let Some(rc) = category(&r) else {
                return Ok(());
            };
            let fits = (lc == "string" && rc == "string") || (lc == "bytea" && rc == "bytea");
            if !fits {
                return Err(mismatch(&op, &l, &r, e.location));
            }
        }
        _ => {}
    }
    Ok(())
}

/// What a walk needs to open a subquery's own scope.
struct Cx<'a> {
    lookup: &'a dyn Fn(&str) -> Option<TableDef>,
    ctes: Vec<Cte>,
}

/// Walk an expression; a subquery is checked in a scope of its own whose
/// parent is this one.
fn walk(n: &pg_query::protobuf::Node, scope: &Scope, cx: &Cx) -> Result<()> {
    // A cancellation point per node: a 200,000-item IN list is walked
    // here for a fifth of a second (RP-202).
    crate::cancel::check()?;
    let Some(node) = n.node.as_ref() else {
        return Ok(());
    };
    // `ParseFuncOrColumn` coerces an aggregate's FILTER before it looks the
    // function up.
    if let N::FuncCall(f) = node {
        if let Some(filter) = f.agg_filter.as_deref() {
            unknown_as_boolean(filter, scope)?;
        }
    }
    match node {
        N::AExpr(e) => {
            if let Err(refused) = check_aexpr(e, scope) {
                // `transformAExprOp` transforms the operands before it
                // looks the operator up, so a literal among them that is
                // no relation is the error first (RP-458).
                let location = take_error_location();
                for s in e.lexpr.iter().chain(e.rexpr.iter()) {
                    regclass::scan(s, scope)?;
                }
                if let Some(l) = location {
                    set_error_location(l);
                }
                return Err(refused);
            }
            for s in e.lexpr.iter().chain(e.rexpr.iter()) {
                walk(s, scope, cx)?;
            }
        }
        N::ColumnRef(c) => regclass::note_reference(c, scope),
        N::BoolExpr(b) => {
            for a in &b.args {
                bool_argument(b, a, scope)?;
                walk(a, scope, cx)?;
            }
        }
        N::List(l) => {
            for a in &l.items {
                walk(a, scope, cx)?;
            }
        }
        N::NullTest(t) => {
            if let Some(a) = t.arg.as_deref() {
                walk(a, scope, cx)?;
            }
        }
        // `transformBooleanTest` transforms its argument, then coerces it
        // with `coerce_to_boolean` under the test's own name (RP-808).
        N::BooleanTest(t) => {
            if let Some(a) = t.arg.as_deref() {
                walk(a, scope, cx)?;
                boolean_test_argument(t, a, scope)?;
            }
        }
        // A cast to or from `"char"` with no path is refused when the
        // statement is analysed, whatever the rows (`i2::"char"` over an
        // empty table too). The call form `int2(x)` is a function call on
        // PostgreSQL when no cast applies, so it is a missing function
        // instead (`func_cast` marks the casts it builds with location -1).
        N::TypeCast(tc) => {
            regclass::cast(tc, scope)?;
            if let (Some(arg), Some(tn)) = (tc.arg.as_deref(), tc.type_name.as_ref()) {
                let target = type_name_of(tn);
                if let Some(e) = operand_type(arg, scope).and_then(|source| {
                    crate::char_cast_refused(&source, &target).map(|e| (source, e))
                }) {
                    let (source, e) = e;
                    if tc.location == -1 {
                        let name: Vec<String> = tn
                            .names
                            .iter()
                            .filter_map(|n| match n.node.as_ref() {
                                Some(N::String(s)) => Some(s.sval.clone()),
                                _ => None,
                            })
                            .collect();
                        set_error_location(tn.location);
                        return Err(Error::UndefinedFunction(format!(
                            "function {}({}) does not exist",
                            name.join("."),
                            crate::display_type(&source)
                        )));
                    }
                    set_error_location(tc.location);
                    return Err(e);
                }
                // Any other explicit cast between built-in types PostgreSQL
                // has no pathway for is refused too, NULL or not:
                // `pg_lsn::int8` is 42846 at the `::`, where it used to
                // answer the number the pg_lsn is carried as. A
                // function-style cast (location -1) is a function call on
                // PostgreSQL, resolved with implicit coercion of its
                // argument, and is left to that.
                // With no cast pathway at all, the call form finds no function
                // either (`func_get_detail` takes a call as a cast only when a
                // coercion path exists): `int8(true)` is 42883 at the call
                // (measured on 15.19).
                if tc.location == -1 {
                    if let Some(source) = operand_type(arg, scope) {
                        if crate::pg_casts::refused(&source, &target) {
                            let name: Vec<String> = tn
                                .names
                                .iter()
                                .filter_map(|n| match n.node.as_ref() {
                                    Some(N::String(s)) => Some(s.sval.clone()),
                                    _ => None,
                                })
                                .collect();
                            set_error_location(tn.location);
                            return Err(Error::UndefinedFunction(format!(
                                "function {}({}) does not exist",
                                name.join("."),
                                crate::display_type(&source)
                            )));
                        }
                    }
                }
                if tc.location >= 0 {
                    if let Some(source) = operand_type(arg, scope) {
                        if crate::pg_casts::refused(&source, &target) {
                            set_error_location(tc.location);
                            return Err(Error::CannotCoerce(format!(
                                "cannot cast type {} to {}",
                                display_type(&source),
                                display_type(&target)
                            )));
                        }
                    }
                }
                walk(arg, scope, cx)?;
            }
        }
        N::ResTarget(r) => {
            if let Some(v) = r.val.as_deref() {
                walk(v, scope, cx)?;
            }
        }
        // An untyped literal among typed arguments is coerced to their type
        // when the statement is analysed, so a literal that is not valid
        // input for it fails then -- evaluated or not (`coalesce(id, 'x')`).
        N::CoalesceExpr(c) => {
            let parts = c.args.iter().collect::<Vec<_>>();
            common_type_check("COALESCE", &parts, scope)?;
            literals_fit(&parts, scope)?;
            for a in &c.args {
                walk(a, scope, cx)?;
            }
        }
        N::MinMaxExpr(m) => {
            let label = if pg_query::protobuf::MinMaxOp::try_from(m.op)
                == Ok(pg_query::protobuf::MinMaxOp::IsGreatest)
            {
                "GREATEST"
            } else {
                "LEAST"
            };
            let parts = m.args.iter().collect::<Vec<_>>();
            common_type_check(label, &parts, scope)?;
            literals_fit(&parts, scope)?;
            for a in &m.args {
                walk(a, scope, cx)?;
            }
        }
        N::CaseExpr(c) => {
            // A searched CASE coerces each WHEN to boolean as it is
            // transformed, before the results' common type is chosen.
            if c.arg.is_none() {
                for w in &c.args {
                    if let Some(N::CaseWhen(w)) = w.node.as_ref() {
                        if let Some(e) = w.expr.as_deref() {
                            unknown_as_boolean(e, scope)?;
                        }
                    }
                }
            }
            // A simple CASE compares its argument to each WHEN with `=`
            // (`transformCaseExpr` builds `CaseTestExpr = warg` by
            // `make_op`), so an untyped WHEN literal takes the argument's
            // type, read by its input function as the WHEN is transformed:
            // `case a when 't'` over an integer is 22P02 at the literal,
            // whether or not a row reaches it (RP-433). A TYPED WHEN is
            // resolved against `pg_operator` as `make_op` resolves it: no
            // `=` between the two types (`case 1 when true`, a text
            // argument against an integer WHEN) is 42883 at the WHEN
            // (RP-439), where the evaluator's comparison was a 0A000.
            // An untyped argument is coerced to text first (`case 'a' when
            // 1` is `text = integer`).
            let arg_type = c.arg.as_deref().and_then(|a| match a.node.as_ref() {
                Some(N::AConst(k))
                    if matches!(k.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
                {
                    Some("text".to_string())
                }
                _ => operand_type(a, scope),
            });
            if let Some(ty) = arg_type {
                for w in &c.args {
                    if let Some(N::CaseWhen(w)) = w.node.as_ref() {
                        if let Some(e) = w.expr.as_deref() {
                            literal_fits(e, &ty)?;
                            let untyped = matches!(e.node.as_ref(), Some(N::AConst(k))
                                if matches!(k.val, Some(pg_query::protobuf::a_const::Val::Sval(_))));
                            let wty = operand_type(e, scope)
                                .filter(|_| !untyped && !user_ops::defines("="));
                            if let Some(wty) = wty {
                                if crate::funcsig::operator_resolves("=", &ty, &wty) == Some(false)
                                {
                                    return Err(mismatch("=", &ty, &wty, w.location));
                                }
                                if crate::funcsig::operator_ambiguous("=", &ty, &wty) {
                                    set_error_location(w.location);
                                    return Err(Error::Sqlstate(
                                        "42725",
                                        format!(
                                            "operator is not unique: {} = {}",
                                            display_type(&ty),
                                            display_type(&wty)
                                        ),
                                    ));
                                }
                            }
                        }
                    }
                }
            }
            // PostgreSQL resolves the ELSE first, then each THEN in order.
            let mut parts: Vec<&pg_query::protobuf::Node> =
                c.defresult.as_deref().into_iter().collect();
            parts.extend(c.args.iter().filter_map(|w| match w.node.as_ref() {
                Some(N::CaseWhen(cw)) => cw.result.as_deref(),
                _ => None,
            }));
            common_type_check("CASE", &parts, scope)?;
            literals_fit(&parts, scope)?;
            for w in &c.args {
                if let Some(N::CaseWhen(w)) = w.node.as_ref() {
                    for s in w.expr.iter().chain(w.result.iter()) {
                        walk(s, scope, cx)?;
                    }
                }
            }
            if let Some(d) = c.defresult.as_deref() {
                walk(d, scope, cx)?;
            }
        }
        N::JoinExpr(j) => {
            for s in j.larg.iter().chain(j.rarg.iter()) {
                walk(s, scope, cx)?;
            }
            // An ON sees only its own join's two sides, and the queries
            // around this one -- never a FROM item beside the join
            // (`transformJoinOnClause` sets the namespace to the join's
            // own). In `FROM e, s JOIN k ON b = k.a`, `b` is `k.b`
            // alone, so the `text = integer` is judged (RP-416).
            if let Some(q) = j.quals.as_deref() {
                let mut on = Scope {
                    tables: Vec::new(),
                    complete: true,
                    unresolved: false,
                    parent: scope.parent,
                    lookup: Some(cx.lookup),
                    ctes: cx.ctes.clone(),
                    merged: Vec::new(),
                    using_aliases: Vec::new(),
                    rowtypes: Vec::new(),
                };
                for side in j.larg.iter().chain(j.rarg.iter()) {
                    on.add(side, &cx.ctes, cx.lookup);
                }
                walk(q, &on, cx)?;
                // Judged in the whole FROM list's scope: a relation missing
                // anywhere in it leaves the literal to the planner.
                unknown_as_boolean(q, scope)?;
            }
        }
        N::SubLink(sl) => {
            if let Some(t) = sl.testexpr.as_deref() {
                walk(t, scope, cx)?;
            }
            any_sublink_operator(sl, scope)?;
            if let Some(N::SelectStmt(sel)) = sl.subselect.as_deref().and_then(|n| n.node.as_ref())
            {
                check_select(sel, &cx.ctes, cx.lookup, Some(scope))?;
            }
        }
        // `__alias_check(name, expr, args...)` (`builtin_alias`): an
        // operator's function called by name over arguments none of its
        // overloads takes is a missing FUNCTION on PostgreSQL, named as
        // called -- not the operator the call was rewritten into.
        N::FuncCall(f) if func_name(f).as_deref() == Some(crate::builtin_alias::ALIAS_CHECK) => {
            if let (Some(N::AConst(c)), Some(expr)) =
                (f.args.first().and_then(|a| a.node.as_ref()), f.args.get(1))
            {
                if let Some(pg_query::protobuf::a_const::Val::Sval(name)) = c.val.as_ref() {
                    let mut call = (**f).clone();
                    call.args = f.args[2..].to_vec();
                    if let Some(types) = call_arg_types(&call, scope) {
                        if types.iter().all(|t| !t.is_empty() && t != "unknown")
                            && crate::funcsig::resolves(&name.sval, &types) == Some(false)
                        {
                            crate::set_error_location(c.location);
                            return Err(Error::UndefinedFunction(format!(
                                "function {}({}) does not exist",
                                name.sval,
                                types
                                    .iter()
                                    .map(|t| crate::display_type(t))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )));
                        }
                    }
                }
                walk(expr, scope, cx)?;
            }
        }
        N::FuncCall(f) => {
            record_builtin_call(f, scope);
            if let Some(w) = f.over.as_deref() {
                check_range_offset(w, scope)?;
                sortable::check_window(w, scope)?;
            }
            sortable::check_call(f, scope)?;
            if let (Some(name), [arg]) = (func_name(f), f.args.as_slice()) {
                if checked_aggregate(&name)
                    && !f.agg_star
                    && !correlated::user_function_named(&name)
                    && !crate::user_agg::is_user_aggregate(&name)
                {
                    if let Some(t) = operand_type(arg, scope) {
                        if crate::funcsig::resolves(&name, std::slice::from_ref(&t)) == Some(false)
                        {
                            set_error_location(f.location);
                            return Err(Error::UndefinedFunction(format!(
                                "function {name}({}) does not exist",
                                crate::display_type(&t)
                            )));
                        }
                    }
                }
            }
            // `min` / `max` have no boolean form (`bool_and` / `bool_or` do
            // that job): a missing function, whatever the query's shape.
            if let (Some(name), [arg]) = (func_name(f), f.args.as_slice()) {
                if matches!(name.as_str(), "min" | "max")
                    && !correlated::user_function_named(&name)
                    && operand_type(arg, scope)
                        .is_some_and(|t| matches!(t.as_str(), "bool" | "boolean"))
                {
                    set_error_location(f.location);
                    return Err(Error::UndefinedFunction(format!(
                        "function {name}(boolean) does not exist"
                    )));
                }
            }
            for a in &f.args {
                walk(a, scope, cx)?;
            }
            regclass::arguments(f, scope)?;
        }
        // A FROM subquery sees the outer query only when it is LATERAL.
        N::RangeSubselect(rs) => {
            if let Some(N::SelectStmt(sel)) = rs.subquery.as_deref().and_then(|n| n.node.as_ref()) {
                check_select(sel, &cx.ctes, cx.lookup, rs.lateral.then_some(scope))?;
            }
        }
        N::AIndirection(ind) => {
            if let Some(arg) = ind.arg.as_deref() {
                walk(arg, scope, cx)?;
                column_notation_on_base_type(ind, arg, scope)?;
            }
        }
        // An expression this walk does not descend into (an `ARRAY[...]`,
        // a `ROW(...)`, an `IS TRUE`) is still transformed by PostgreSQL.
        _ => regclass::scan(n, scope)?,
    }
    Ok(())
}

/// `(expr).field` or `(expr).*` over a value of a base type is refused when
/// the statement is analysed, whatever the rows: `transformIndirection` hands
/// a field name to `ParseFuncOrColumn`, whose column notation over a type
/// that is not composite is 42809 `column notation .t applied to type
/// integer, which is not a composite type` at the operand
/// (`exprLocation`), and `ExpandRowReference` refuses a star with `type
/// integer is not composite`, with no position. A function with one OUT
/// parameter returns that parameter's type, a scalar (`(gto()).t`, RP-483).
///
/// Only a type known to be a built-in base type (or an array, never
/// composite) is judged; a record, a composite, a domain over one, a user
/// type or a type this pass cannot settle is left to the planner. A `$n`
/// is left too: the server's own rewrites bind values through parameters
/// whose declared type is not always the value's.
fn column_notation_on_base_type(
    ind: &pg_query::protobuf::AIndirection,
    arg: &pg_query::protobuf::Node,
    scope: &Scope,
) -> Result<()> {
    let field = match ind.indirection.first().and_then(|i| i.node.as_ref()) {
        Some(N::String(s)) => Some(s.sval.as_str()),
        Some(N::AStar(_)) => None,
        _ => return Ok(()),
    };
    let Some(ty) = notation_operand_type(arg, scope) else {
        return Ok(());
    };
    let ty = crate::domains::domain_base(&ty).unwrap_or(ty);
    if crate::pgtypes::PSEUDO_TYPES.iter().any(|(_, n)| *n == ty)
        || crate::pgtypes::oid_of_name(&ty).is_none()
    {
        return Ok(());
    }
    let shown = crate::display_type(&ty);
    match field {
        Some(field) => {
            if let Some(loc) = crate::expr_location(arg) {
                set_error_location(loc);
            }
            Err(Error::Sqlstate(
                "42809",
                format!(
                    "column notation .{field} applied to type {shown}, which is not a composite type"
                ),
            ))
        }
        None => Err(Error::Sqlstate(
            "42809",
            format!("type {shown} is not composite"),
        )),
    }
}

/// The type of a column notation's operand, where this pass knows it: a
/// user function's call is of its declared result type (its one OUT
/// parameter's, for a function with one), a cast of the type it names, an
/// `ARRAY[...]` of its elements' type's array, anything else as
/// [`operand_type`] types it.
fn notation_operand_type(n: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    match n.node.as_ref()? {
        N::ParamRef(_) => None,
        N::FuncCall(f) => {
            let name = func_name(f)?;
            if !correlated::user_function_named(&name) {
                if let Some(t) = operand_type(n, scope) {
                    return Some(t);
                }
                // A set-returning built-in's element type (`unnest`,
                // `generate_series`), which `operand_type` leaves alone.
                if f.over.is_some() || f.agg_star || f.funcname.len() > 2 {
                    return None;
                }
                let args = f
                    .args
                    .iter()
                    .map(|a| match a.node.as_ref() {
                        Some(N::AConst(c))
                            if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
                        {
                            Some(String::new())
                        }
                        _ => notation_operand_type(a, scope),
                    })
                    .collect::<Option<Vec<_>>>()?;
                return crate::funcsig::set_result_type(&name, &args);
            }
            if f.over.is_some() || f.agg_star || !f.agg_order.is_empty() {
                return None;
            }
            let u = correlated::user_function_for(&name, &f.args)?;
            (u.columns.len() <= 1).then(|| u.return_type.clone())
        }
        N::TypeCast(tc) => Some(type_name_of(tc.type_name.as_ref()?)),
        // A multidimensional array is of the same array type as its
        // elements (`ARRAY[ARRAY[1]]` is `integer[]`).
        N::AArrayExpr(a) => {
            let mut types = a.elements.iter().map(|e| notation_operand_type(e, scope));
            let first = types.next()??;
            if !types.all(|t| t.as_deref() == Some(first.as_str())) {
                return None;
            }
            Some(if first.ends_with("[]") {
                first
            } else {
                format!("{first}[]")
            })
        }
        _ => operand_type(n, scope),
    }
}

fn check_select(
    s: &pg_query::protobuf::SelectStmt,
    outer_ctes: &[Cte],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    parent: Option<&Scope>,
) -> Result<()> {
    let ctes = with_ctes(s.with_clause.as_ref(), outer_ctes, lookup, parent)?;
    // A set operation's arms each have their own FROM, under the same parent.
    if let Some(l) = s.larg.as_deref() {
        check_select(l, &ctes, lookup, parent)?;
    }
    if let Some(r) = s.rarg.as_deref() {
        check_select(r, &ctes, lookup, parent)?;
    }
    sibling_reference(s, parent)?;
    let scope = Scope::new(&s.from_clause, &ctes, lookup, parent);
    let cx = Cx { lookup, ctes };
    for f in &s.from_clause {
        walk(f, &scope, &cx)?;
    }
    for t in &s.target_list {
        walk(t, &scope, &cx)?;
    }
    if let Some(w) = s.where_clause.as_deref() {
        // Only at the top level: in a subquery an aggregate over the OUTER
        // query's columns (`WHERE x.k = min(d.id)`) is the outer query's.
        if parent.is_none() {
            no_aggregate_in_where(w)?;
        }
        walk(w, &scope, &cx)?;
        clause_is_boolean(w, &scope, "WHERE")?;
    }
    if let Some(h) = s.having_clause.as_deref() {
        having_names_resolve(h, &scope)?;
        walk(h, &scope, &cx)?;
        clause_is_boolean(h, &scope, "HAVING")?;
    }
    for w in &s.window_clause {
        if let Some(N::WindowDef(w)) = w.node.as_ref() {
            check_range_offset(w, &scope)?;
        }
    }
    for n in s.sort_clause.iter().filter_map(|n| match n.node.as_ref() {
        Some(N::SortBy(b)) => b.node.as_deref(),
        _ => None,
    }) {
        walk(n, &scope, &cx)?;
    }
    if s.op == pg_query::protobuf::SetOperation::SetopNone as i32 {
        sortable::check_clauses(s, &scope)?;
    } else {
        sortable::check_set_operation(s, &cx.ctes, lookup, parent)?;
    }
    Ok(())
}

/// A non-LATERAL FROM-less subquery in a FROM list cannot see its sibling
/// FROM items: `FROM t x, (SELECT x.a) s` is PostgreSQL's 42P01 `invalid
/// reference to FROM-clause entry for table "x"` with its HINT, at the
/// reference. (Planned as it stood, it reached the lowering as an unknown
/// column, 42703.) A name an enclosing query has is that query's, and a
/// subquery with a FROM of its own is left to the planner.
fn sibling_reference(s: &pg_query::protobuf::SelectStmt, parent: Option<&Scope>) -> Result<()> {
    fn item_name(n: &pg_query::protobuf::Node, out: &mut Vec<String>) {
        match n.node.as_ref() {
            Some(N::RangeVar(rv)) => out.push(
                rv.alias
                    .as_ref()
                    .map(|a| a.aliasname.clone())
                    .unwrap_or_else(|| rv.relname.clone()),
            ),
            Some(N::RangeSubselect(rs)) => {
                out.extend(rs.alias.as_ref().map(|a| a.aliasname.clone()))
            }
            Some(N::JoinExpr(j)) => {
                for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                    item_name(side, out);
                }
            }
            _ => {}
        }
    }
    let mut siblings = Vec::new();
    for item in &s.from_clause {
        item_name(item, &mut siblings);
    }
    let in_outer = |name: &str| {
        let mut p = parent;
        while let Some(sc) = p {
            if sc.tables.iter().any(|(n, _)| n == name) {
                return true;
            }
            p = sc.parent;
        }
        false
    };
    for item in &s.from_clause {
        let Some(N::RangeSubselect(rs)) = item.node.as_ref() else {
            continue;
        };
        if rs.lateral {
            continue;
        }
        let Some(N::SelectStmt(q)) = rs.subquery.as_deref().and_then(|n| n.node.as_ref()) else {
            continue;
        };
        if !q.from_clause.is_empty() || q.larg.is_some() {
            continue;
        }
        for t in &q.target_list {
            let Some(inner) = t.node.as_ref() else {
                continue;
            };
            for (n, _, _, _) in inner.nodes() {
                let pg_query::NodeRef::ColumnRef(c) = n else {
                    continue;
                };
                if c.fields.len() != 2 {
                    continue;
                }
                let Some(N::String(q)) = c.fields[0].node.as_ref() else {
                    continue;
                };
                let name = q.sval.as_str();
                if siblings.iter().any(|n| n == name) && !in_outer(name) {
                    set_error_location(c.location);
                    return Err(Error::Sqlstate(
                        "42P01",
                        format!(
                            "invalid reference to FROM-clause entry for table \"{name}\"\nHint: There is an entry for table \"{name}\", but it cannot be referenced from this part of the query."
                        ),
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A `RANGE` frame with an offset, as `transformFrameOffset` checks it:
/// exactly one ORDER BY column (42P20); a number column takes a number
/// offset, a date/time column an interval, and any other is 0A000.
fn check_range_offset(w: &pg_query::protobuf::WindowDef, scope: &Scope) -> Result<()> {
    const RANGE: i32 = 0x00002;
    if w.frame_options & RANGE == 0 {
        return Ok(());
    }
    let offsets: Vec<&pg_query::protobuf::Node> = w
        .start_offset
        .iter()
        .chain(w.end_offset.iter())
        .map(|b| &**b)
        .collect();
    if offsets.is_empty() {
        return Ok(());
    }
    if !w.refname.is_empty() {
        return Ok(());
    }
    if w.order_clause.len() != 1 {
        set_error_location(w.location);
        return Err(Error::Sqlstate(
            "42P20",
            "RANGE with offset PRECEDING/FOLLOWING requires exactly one ORDER BY column".into(),
        ));
    }
    let Some(N::SortBy(b)) = w.order_clause[0].node.as_ref() else {
        return Ok(());
    };
    let Some(ordering) = b.node.as_deref().and_then(|n| operand_type(n, scope)) else {
        return Ok(());
    };
    let datetime = matches!(
        ordering.as_str(),
        "date" | "timestamp" | "timestamptz" | "time" | "timetz"
    );
    for off in offsets {
        let location = crate::expr_location(off).unwrap_or(-1);
        match category(&ordering) {
            Some("numeric") => literal_fits(off, &ordering)?,
            _ if datetime => {
                if let Some(t) = operand_type(off, scope) {
                    if t != "interval" {
                        set_error_location(location);
                        return Err(Error::Sqlstate(
                            "0A000",
                            format!(
                                "RANGE with offset PRECEDING/FOLLOWING is not supported for column type {} and offset type {}\nHint: Cast the offset value to an appropriate type.",
                                display_type(&ordering),
                                display_type(&t)
                            ),
                        ));
                    }
                }
            }
            Some(_) => {
                set_error_location(location);
                return Err(Error::Sqlstate(
                    "0A000",
                    format!(
                        "RANGE with offset PRECEDING/FOLLOWING is not supported for column type {}",
                        display_type(&ordering)
                    ),
                ));
            }
            None => {}
        }
    }
    Ok(())
}

/// 42803 `aggregate functions are not allowed in WHERE`, at the call: the
/// WHERE runs before any aggregate. A subquery's own aggregates are its own.
fn no_aggregate_in_where(n: &pg_query::protobuf::Node) -> Result<()> {
    let mut found: Option<i32> = None;
    fn visit(n: &pg_query::protobuf::Node, found: &mut Option<i32>) {
        if found.is_some() {
            return;
        }
        match n.node.as_ref() {
            Some(N::FuncCall(f)) if f.over.is_some() => {
                *found = Some(-1 - f.location);
            }
            Some(N::FuncCall(f)) => {
                if crate::is_aggregate_call(f)
                    && !func_name(f).is_some_and(|n| correlated::user_function_named(&n))
                {
                    *found = Some(f.location);
                    return;
                }
                for a in &f.args {
                    visit(a, found);
                }
            }
            Some(N::AExpr(e)) => {
                for s in e.lexpr.iter().chain(e.rexpr.iter()) {
                    visit(s, found);
                }
            }
            Some(N::BoolExpr(b)) => b.args.iter().for_each(|a| visit(a, found)),
            Some(N::List(l)) => l.items.iter().for_each(|a| visit(a, found)),
            Some(N::NullTest(t)) => t.arg.iter().for_each(|a| visit(a, found)),
            Some(N::TypeCast(c)) => c.arg.iter().for_each(|a| visit(a, found)),
            Some(N::CoalesceExpr(c)) => c.args.iter().for_each(|a| visit(a, found)),
            _ => {}
        }
    }
    visit(n, &mut found);
    match found {
        // A window function (its location stored as `-1 - location`).
        Some(window) if window < 0 => {
            set_error_location(-1 - window);
            Err(Error::Windowing(
                "window functions are not allowed in WHERE".into(),
            ))
        }
        Some(location) => {
            set_error_location(location);
            Err(Error::Grouping(
                "aggregate functions are not allowed in WHERE".into(),
            ))
        }
        None => Ok(()),
    }
}

/// The aggregates whose overloads are all numeric / interval / boolean /
/// bit: an argument of a known type none of them takes is 42883 at plan
/// time, where the lowering would aggregate it anyway (`sum(text)` was 0).
fn checked_aggregate(name: &str) -> bool {
    matches!(
        name,
        "sum"
            | "avg"
            | "stddev"
            | "stddev_pop"
            | "stddev_samp"
            | "variance"
            | "var_pop"
            | "var_samp"
            | "bool_and"
            | "bool_or"
            | "every"
            | "bit_and"
            | "bit_or"
            | "bit_xor"
    )
}

/// A built-in call: its name and the argument types of the overload chosen.
pub(crate) type BuiltinCall = (String, Option<Vec<String>>);

thread_local! {
    /// While `builtin_calls` walks a statement: the built-in calls it met.
    static BUILTIN_CALLS: std::cell::RefCell<Option<Vec<BuiltinCall>>> =
        const { std::cell::RefCell::new(None) };
}

/// The argument types of a built-in call, for choosing its overload: an
/// untyped string literal is `""` (unknown), and an argument that is itself
/// a built-in call takes the result type of the overload it resolves to
/// (`abs(length('ab'))` calls `abs(int4)`). Kept out of `operand_type`, so
/// the operator check is not moved by it.
fn call_arg_types(f: &pg_query::protobuf::FuncCall, scope: &Scope) -> Option<Vec<String>> {
    f.args
        .iter()
        .map(|a| match a.node.as_ref() {
            Some(N::AConst(c))
                if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))) =>
            {
                Some(String::new())
            }
            Some(N::FuncCall(inner))
                if inner.agg_order.is_empty()
                    && inner.over.is_none()
                    && !inner.agg_star
                    && !inner.func_variadic =>
            {
                operand_type(a, scope).or_else(|| {
                    let name = func_name(inner)?;
                    if correlated::user_function_named(&name)
                        || inner.funcname.len() > 2
                        || !crate::funcsig::is_builtin_function_name(&name)
                    {
                        return None;
                    }
                    let args = call_arg_types(inner, scope)?;
                    if args.iter().any(String::is_empty) {
                        // An untyped literal: `func_select_candidate`'s
                        // unknown-argument rules (`length('ab')` is text's).
                        let args: Vec<String> = args
                            .into_iter()
                            .map(|a| if a.is_empty() { "unknown".into() } else { a })
                            .collect();
                        return crate::funcsig::resolved_result_type(&name, &args);
                    }
                    crate::funcsig::selected_result_type(&name, &args)
                })
            }
            _ => operand_type(a, scope),
        })
        .collect()
}

fn record_builtin_call(f: &pg_query::protobuf::FuncCall, scope: &Scope) {
    if BUILTIN_CALLS.with(|c| c.borrow().is_none()) {
        return;
    }
    let Some(name) = func_name(f) else {
        return;
    };
    if correlated::user_function_named(&name)
        || f.funcname.len() > 2
        || !crate::funcsig::is_builtin_function_name(&name)
    {
        return;
    }
    let chosen = call_arg_types(f, scope).and_then(|a| crate::funcsig::selected_args(&name, &a));
    BUILTIN_CALLS.with(|c| {
        if let Some(v) = c.borrow_mut().as_mut() {
            v.push((name, chosen));
        }
    });
}

/// The built-in function calls of a SELECT (its select list, FROM and
/// WHERE, subqueries included) and of an UPDATE / DELETE's WHERE: each
/// name with the argument types of the overload PostgreSQL's
/// `func_select_candidate` picks, `None` where that is not determined.
pub(crate) fn builtin_calls(
    node: &N,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Vec<BuiltinCall> {
    BUILTIN_CALLS.with(|c| *c.borrow_mut() = Some(Vec::new()));
    let _ = check(node, lookup);
    let _ = walk_other_clauses(node, lookup);
    BUILTIN_CALLS.with(|c| c.borrow_mut().take().unwrap_or_default())
}

/// The clauses `check` leaves alone but a call can sit in: a SELECT's
/// ORDER BY / GROUP BY / HAVING / VALUES, an UPDATE's SET, an INSERT's
/// VALUES or query, and a RETURNING list. Walked only to collect calls.
fn walk_other_clauses(node: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<()> {
    let cx = Cx {
        lookup,
        ctes: Vec::new(),
    };
    let relation = |r: &Option<pg_query::protobuf::RangeVar>| -> Vec<pg_query::protobuf::Node> {
        r.iter()
            .map(|r| pg_query::protobuf::Node {
                node: Some(N::RangeVar(r.clone())),
            })
            .collect()
    };
    match node {
        N::SelectStmt(s) => {
            let scope = Scope::new(&s.from_clause, &[], lookup, None);
            for n in s.sort_clause.iter().filter_map(|n| match n.node.as_ref() {
                Some(N::SortBy(b)) => b.node.as_deref(),
                _ => None,
            }) {
                walk(n, &scope, &cx)?;
            }
            for n in s
                .group_clause
                .iter()
                .chain(s.having_clause.iter().map(|b| &**b))
                .chain(s.values_lists.iter())
            {
                walk(n, &scope, &cx)?;
            }
        }
        N::UpdateStmt(u) => {
            let mut items = relation(&u.relation);
            items.extend(u.from_clause.iter().cloned());
            let scope = Scope::new(&items, &[], lookup, None);
            for n in u.target_list.iter().chain(u.returning_list.iter()) {
                walk(n, &scope, &cx)?;
            }
        }
        N::DeleteStmt(d) => {
            let scope = Scope::new(&relation(&d.relation), &[], lookup, None);
            for n in &d.returning_list {
                walk(n, &scope, &cx)?;
            }
        }
        N::InsertStmt(i) => {
            if let Some(N::SelectStmt(sel)) = i.select_stmt.as_deref().and_then(|n| n.node.as_ref())
            {
                let _ = check_select(sel, &[], lookup, None);
                walk_other_clauses(&N::SelectStmt(sel.clone()), lookup)?;
            }
            let scope = Scope::new(&relation(&i.relation), &[], lookup, None);
            for n in &i.returning_list {
                walk(n, &scope, &cx)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// 42883 for a comparison with no operator between its operands' types.
pub(crate) fn check(node: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<()> {
    regclass::analysing(|| check_in(node, &[], lookup))
}

/// [`check`] with the CTEs of an enclosing WITH in view: a data-modifying
/// CTE sees the ones before it, as a SELECT's does.
fn check_in(node: &N, outer: &[Cte], lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<()> {
    let relation = |r: &Option<pg_query::protobuf::RangeVar>| {
        r.as_ref().map(|r| pg_query::protobuf::Node {
            node: Some(N::RangeVar(r.clone())),
        })
    };
    match node {
        N::SelectStmt(s) => check_select(s, outer, lookup, None),
        // `transformInsertStmt` analyses the WITH, then the SELECT as a
        // query of its own that sees those CTEs. A VALUES list is checked
        // elsewhere.
        N::InsertStmt(i) => {
            let ctes = with_ctes(i.with_clause.as_ref(), outer, lookup, None)?;
            match i.select_stmt.as_deref().and_then(|n| n.node.as_ref()) {
                Some(N::SelectStmt(sel)) => check_select(sel, &ctes, lookup, None),
                _ => Ok(()),
            }
        }
        N::UpdateStmt(u) => {
            let ctes = with_ctes(u.with_clause.as_ref(), outer, lookup, None)?;
            let items: Vec<_> = relation(&u.relation)
                .into_iter()
                .chain(u.from_clause.iter().cloned())
                .collect();
            let scope = Scope::new(&items, &ctes, lookup, None);
            let cx = Cx { lookup, ctes };
            match u.where_clause.as_deref() {
                Some(w) => {
                    no_aggregate_in_where(w)?;
                    walk(w, &scope, &cx)?;
                    clause_is_boolean(w, &scope, "WHERE")
                }
                None => Ok(()),
            }
        }
        N::DeleteStmt(d) => {
            let ctes = with_ctes(d.with_clause.as_ref(), outer, lookup, None)?;
            let items: Vec<_> = relation(&d.relation)
                .into_iter()
                .chain(d.using_clause.iter().cloned())
                .collect();
            let scope = Scope::new(&items, &ctes, lookup, None);
            let cx = Cx { lookup, ctes };
            match d.where_clause.as_deref() {
                Some(w) => {
                    no_aggregate_in_where(w)?;
                    walk(w, &scope, &cx)?;
                    clause_is_boolean(w, &scope, "WHERE")
                }
                None => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// The CTEs in view inside a WITH clause's statement: `outer`'s, then the
/// clause's own. Each CTE's query is checked as `transformWithClause`
/// analyses it, before the statement it belongs to: a plain WITH's query
/// sees the CTEs before it; under WITH RECURSIVE every name of the clause
/// is in view (untyped, as the recursion's columns are not known here).
fn with_ctes(
    w: Option<&pg_query::protobuf::WithClause>,
    outer: &[Cte],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    parent: Option<&Scope>,
) -> Result<Vec<Cte>> {
    let mut ctes = outer.to_vec();
    let Some(w) = w else {
        return Ok(ctes);
    };
    let defs: Vec<&pg_query::protobuf::CommonTableExpr> = w
        .ctes
        .iter()
        .filter_map(|c| match c.node.as_ref() {
            Some(N::CommonTableExpr(c)) => Some(&**c),
            _ => None,
        })
        .collect();
    if w.recursive {
        ctes.extend(defs.iter().map(|c| (c.ctename.clone(), None)));
    }
    for c in defs {
        match c.ctequery.as_deref().and_then(|n| n.node.as_ref()) {
            Some(N::SelectStmt(q)) => check_select(q, &ctes, lookup, parent)?,
            Some(other) => check_in(other, &ctes, lookup)?,
            None => {}
        }
        if !w.recursive {
            let query = match c.ctequery.as_deref().and_then(|n| n.node.as_ref()) {
                Some(N::SelectStmt(q)) => Some(((**q).clone(), c.aliascolnames.clone())),
                _ => None,
            };
            ctes.push((c.ctename.clone(), query));
        }
    }
    Ok(ctes)
}

/// Does an untyped string literal read as `ty`? Its cast's error at the
/// literal when not; anything else (or a type this does not judge) passes.
fn literal_fits(n: &pg_query::protobuf::Node, ty: &str) -> Result<()> {
    use pg_query::protobuf::a_const::Val;
    if !matches!(
        ty,
        "int2" | "int4" | "int8" | "numeric" | "float4" | "float8" | "bool" | "date" | "uuid"
    ) {
        return Ok(());
    }
    if let Some(N::AConst(c)) = n.node.as_ref() {
        if let Some(Val::Sval(s)) = c.val.as_ref() {
            if let Err(e) = crate::cast_value(bson::Bson::String(s.sval.clone()), ty) {
                set_error_location(c.location);
                return Err(e);
            }
        }
    }
    Ok(())
}

/// A bare string literal or NULL: of type `unknown` where it is written.
fn is_untyped_literal(n: &pg_query::protobuf::Node) -> bool {
    matches!(n.node.as_ref(), Some(N::AConst(c))
        if c.isnull || matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))))
}

/// The type of a subquery's first output column, as a scalar subquery
/// over it is typed: `Some(None)` when every arm gives an untyped literal
/// or NULL there -- which the query around it reads as `text`, as
/// `resolveTargetListUnknowns` resolves a subquery's own targets and a set
/// operation its all-unknown columns -- and `None` when this cannot tell.
/// Arms of different known types are `None` too: their common type is the
/// planner's to judge.
fn first_output_type(
    sub: &pg_query::protobuf::SelectStmt,
    scope: &Scope,
) -> Option<Option<String>> {
    if sub.op != pg_query::protobuf::SetOperation::SetopNone as i32 {
        let left = first_output_type(sub.larg.as_deref()?, scope)?;
        let right = first_output_type(sub.rarg.as_deref()?, scope)?;
        return match (left, right) {
            (None, t) | (t, None) => Some(t),
            (Some(l), Some(r)) => (l == r).then_some(Some(l)),
        };
    }
    if sub.with_clause.is_some() || !sub.values_lists.is_empty() {
        return None;
    }
    let Some(N::ResTarget(rt)) = sub.target_list.first()?.node.as_ref() else {
        return None;
    };
    let val = rt.val.as_deref()?;
    if is_untyped_literal(val) {
        return Some(None);
    }
    let inner = Scope::new(&sub.from_clause, &scope.ctes, scope.lookup?, Some(scope));
    operand_type(val, &inner).map(Some)
}

/// The type several branches share: their known types agreeing (numbers
/// meeting as numeric), untyped literals ignored.
fn common_type(parts: &[&pg_query::protobuf::Node], scope: &Scope) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    let mut out: Option<String> = None;
    for p in parts {
        let t = match p.node.as_ref() {
            Some(N::AConst(c)) if c.isnull || matches!(c.val, Some(Val::Sval(_))) => continue,
            _ => operand_type(p, scope)?,
        };
        out = Some(match out {
            None => t,
            Some(o) => select_common(&o, &t)?,
        });
    }
    // Every input an untyped literal (or NULL): `select_common_type`
    // resolves them as text.
    match out {
        None if !parts.is_empty() => Some("text".into()),
        out => out,
    }
}

/// A bare column in HAVING that no FROM item (nor an enclosing query) has is
/// 42703, which PostgreSQL reports before any grouping error.
fn having_names_resolve(h: &pg_query::protobuf::Node, scope: &Scope) -> Result<()> {
    const SYSTEM: &[&str] = &["tableoid", "ctid", "xmin", "xmax", "cmin", "cmax", "oid"];
    let mut probe = h.clone();
    let mut missing: Option<(String, i32)> = None;
    let _ = walk_expr(&mut probe, &mut |n| {
        if missing.is_some() {
            return Ok(());
        }
        // A subquery has a scope of its own.
        if matches!(n.node.as_ref(), Some(N::SubLink(_))) {
            n.node = None;
            return Ok(());
        }
        if let Some(N::ColumnRef(c)) = n.node.as_ref() {
            if let [f] = c.fields.as_slice() {
                if let Some(N::String(name)) = f.node.as_ref() {
                    if !SYSTEM.contains(&name.sval.as_str())
                        && scope.knows(&name.sval) == Some(false)
                        && !scope.names_item(&name.sval)
                    {
                        missing = Some((name.sval.clone(), c.location));
                    }
                }
            }
        }
        Ok(())
    });
    match missing {
        Some((name, location)) => {
            if location >= 0 {
                set_error_location(location);
            }
            Err(Error::UndefinedColumn(name))
        }
        None => Ok(()),
    }
}

/// PostgreSQL's `select_common_type` over the parts of a CASE / COALESCE /
/// GREATEST / LEAST, in its order: the first typed part sets the category,
/// and a later one of ANOTHER category is 42804 at that part (where this
/// server answered with whichever value came first). Untyped literals and
/// NULLs take the common type; a part of a type this cannot judge skips the
/// check.
///
/// A whole row is of its relation's composite type (category C), a
/// `ROW(...)` or a subquery's whole row of `record` (a pseudo-type,
/// category P), so the two do not meet (`greatest(fs, (2, 'a'::text))`).
/// Once every part is of one category, each is coerced to the common
/// type (`coerce_to_common_type`), and a composite has no implicit cast to
/// another: `coalesce(fs, ft)` is 42846 at `ft`, after the category pass.
fn common_type_check(
    label: &str,
    parts: &[&pg_query::protobuf::Node],
    scope: &Scope,
) -> Result<()> {
    use pg_query::protobuf::a_const::Val;
    let mut first: Option<(String, &'static str)> = None;
    let mut typed: Vec<(&pg_query::protobuf::Node, String, &'static str)> = Vec::new();
    for p in parts {
        if matches!(p.node.as_ref(), Some(N::AConst(c)) if c.isnull || matches!(c.val, Some(Val::Sval(_))))
        {
            continue;
        }
        let Some(t) = operand_type(p, scope).or_else(|| row_part_type(p, scope)) else {
            return Ok(());
        };
        let Some(cat) = part_category(&t) else {
            return Ok(());
        };
        match &first {
            None => first = Some((t.clone(), cat)),
            Some((pt, pc)) if *pc != cat => {
                if let Some(loc) = part_location(p) {
                    set_error_location(loc);
                }
                return Err(Error::DatatypeMismatch(format!(
                    "{label} types {} and {} cannot be matched",
                    display_type(pt),
                    display_type(&t)
                )));
            }
            Some(_) => {}
        }
        typed.push((p, t, cat));
    }
    if let Some((pt, "composite")) = &first {
        // One composite type by its oid, however its name was written.
        let oid = |t: &str| crate::user_composite(t).map(|(oid, _)| oid);
        let common = oid(pt);
        if let Some((p, t, _)) = typed
            .iter()
            .find(|(_, t, _)| common.is_some() && oid(t) != common)
        {
            if let Some(loc) = part_location(p) {
                set_error_location(loc);
            }
            return Err(Error::CannotCoerce(format!(
                "{label} could not convert type {} to {}",
                display_type(t),
                display_type(pt)
            )));
        }
    }
    Ok(())
}

/// The type of a part `operand_type` leaves untyped that is a row: a
/// whole-row reference, or a `ROW(...)` (`record`).
fn row_part_type(p: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    match p.node.as_ref()? {
        N::ColumnRef(c) => scope.whole_row_type(c),
        N::RowExpr(_) => Some("record".into()),
        _ => None,
    }
}

/// The row type of an operand that is a row: a table's or a view's whole
/// row (its composite type), a `ROW(...)` or a FROM subquery's whole row
/// (`record`), or a value of a composite type (`row(1)::ct`, a column of
/// one). `None` for anything else, or what this cannot type.
fn row_operand_type(n: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    row_part_type(n, scope).or_else(|| {
        operand_type_declared(n, scope).filter(|t| t == "record" || user_composite(t).is_some())
    })
}

/// A row beside a typed operand that is not a row has no operator. The
/// operators that take a row are `record`'s comparisons, each against
/// another row, and the polymorphic ones; of those only `anynonarray ||
/// text` (and `text || anynonarray`) can take a row beside a scalar, and
/// `||` beside an array may be `anycompatible || anycompatiblearray`. So
/// `fs = 1`, `(1,2) < 1::bigint`, `fs <@ int4range(1,2)`, `nullif(fs, 1)`
/// and `fs in (fs, 1)` are 42883 naming the two types as written (a domain
/// by its own name) at the operator, as `make_op` reports them when the
/// statement is analysed -- over no row too. The lowering compared a
/// document with the scalar at run time (0A000 `comparing document with
/// int32`) or, over no row or a NULL, answered; `fs || 1` concatenated.
/// An untyped literal is left alone: it is read as the row's type.
fn row_against_non_row(
    op: &str,
    l: &pg_query::protobuf::Node,
    r: &pg_query::protobuf::Node,
    location: i32,
    scope: &Scope,
) -> Result<()> {
    // A scalar subquery of more than one column is `transformSubLink`'s
    // 42601 first, before any operator is looked up.
    let wide = |n: &pg_query::protobuf::Node| {
        matches!(n.node.as_ref(), Some(N::SubLink(sl))
            if matches!(sl.subselect.as_deref().and_then(|s| s.node.as_ref()),
                Some(N::SelectStmt(s)) if s.target_list.len() != 1))
    };
    if wide(l) || wide(r) {
        return Ok(());
    }
    let (lr, rr) = (row_operand_type(l, scope), row_operand_type(r, scope));
    let (Some(lt), Some(rt)) = (
        lr.clone().or_else(|| operand_type_declared(l, scope)),
        rr.clone().or_else(|| operand_type_declared(r, scope)),
    ) else {
        return Ok(());
    };
    if lr.is_some() == rr.is_some() {
        return Ok(());
    }
    let other = if lr.is_some() { &rt } else { &lt };
    if other == "unknown" {
        return Ok(());
    }
    if op == "||" {
        let base = crate::domains::domain_base(other).unwrap_or_else(|| other.clone());
        if category(&base) == Some("string") || base.ends_with("[]") {
            return Ok(());
        }
    }
    let op = if op == "!=" { "<>" } else { op };
    Err(mismatch(op, &lt, &rt, location))
}

/// A part's type category: a built-in one `category` knows, `record`'s
/// pseudo-type category, or a composite type's (a table's or a view's row
/// type, a `CREATE TYPE ... AS`).
fn part_category(t: &str) -> Option<&'static str> {
    if t == "record" {
        return Some("pseudo");
    }
    category(t).or_else(|| crate::user_composite(t).map(|_| "composite"))
}

/// Where `select_common_type` reports a part: `exprLocation` of the part
/// as analysed. A `ROW(...)` cast to a composite becomes a new RowExpr at
/// the cast (`coerce_record_to_complex`), so `row(0, 'a')::fs` is
/// reported at its `::`.
fn part_location(p: &pg_query::protobuf::Node) -> Option<i32> {
    if let Some(N::TypeCast(tc)) = p.node.as_ref() {
        if matches!(
            tc.arg.as_deref().and_then(|a| a.node.as_ref()),
            Some(N::RowExpr(_))
        ) && tc.location >= 0
        {
            return Some(tc.location);
        }
    }
    expr_location(p)
}

/// `coerce_to_boolean`: a clause of a typed, non-boolean built-in type --
/// `WHERE a::text`, `WHERE null::int` -- is 42804 at the clause's leftmost
/// token, after the clause's own expression was analysed (PostgreSQL's
/// `transformWhereClause` transforms it first). Lowered as it stood, the
/// clause matched no row and the statement answered an empty result. An
/// untyped literal that is no boolean is `boolin`'s 22P02; NULL, or a type
/// this does not judge, is left alone.
fn clause_is_boolean(n: &pg_query::protobuf::Node, scope: &Scope, clause: &str) -> Result<()> {
    // An untyped literal is read by `boolin` (RP-424, `bool_clauses`).
    unknown_as_boolean(n, scope)?;
    let Some(t) = operand_type(n, scope) else {
        return Ok(());
    };
    if matches!(t.as_str(), "bool" | "boolean") || category(&t).is_none() {
        return Ok(());
    }
    let loc = crate::srf_place::leftmost(n);
    if loc >= 0 {
        set_error_location(loc);
    }
    Err(Error::DatatypeMismatch(format!(
        "argument of {clause} must be type boolean, not type {}",
        display_type(&t)
    )))
}

/// `coerce_to_boolean` of an untyped literal: 22P02 when `boolin` cannot
/// read it (`bool_clauses`). Not in a scope whose FROM list has a relation
/// the lookup did not find, whose own error comes first; the planner reads
/// the literal there.
fn unknown_as_boolean(n: &pg_query::protobuf::Node, scope: &Scope) -> Result<()> {
    if scope.relation_unresolved() {
        return Ok(());
    }
    crate::bool_clauses::check(n)
}

/// An argument of AND / OR / NOT must be boolean: a typed operand of
/// another built-in type is 42804 at that operand.
fn bool_argument(
    b: &pg_query::protobuf::BoolExpr,
    arg: &pg_query::protobuf::Node,
    scope: &Scope,
) -> Result<()> {
    use pg_query::protobuf::BoolExprType;
    unknown_as_boolean(arg, scope)?;
    let Some(t) = operand_type(arg, scope) else {
        return Ok(());
    };
    if matches!(t.as_str(), "bool" | "boolean") || category(&t).is_none() {
        return Ok(());
    }
    if let Some(loc) = expr_location(arg) {
        set_error_location(loc);
    }
    Err(Error::DatatypeMismatch(format!(
        "argument of {} must be type boolean, not type {}",
        match BoolExprType::try_from(b.boolop) {
            Ok(BoolExprType::AndExpr) => "AND",
            Ok(BoolExprType::OrExpr) => "OR",
            _ => "NOT",
        },
        display_type(&t)
    )))
}

/// `x IS [NOT] TRUE / FALSE / UNKNOWN`: `transformBooleanTest` coerces `x`
/// to boolean naming the test, so a typed operand of another built-in type
/// is 42804 at the operand (`select 1 is true`). Lowered as it stood, the
/// test was `coalesce(x, false)` over a number and answered `false`, and
/// `IS UNKNOWN` was `IS NULL`, true for any NULL. An untyped literal is
/// `boolin`'s input (`'a' is true` is 22P02).
fn boolean_test_argument(
    t: &pg_query::protobuf::BooleanTest,
    arg: &pg_query::protobuf::Node,
    scope: &Scope,
) -> Result<()> {
    use pg_query::protobuf::BoolTestType as B;
    unknown_as_boolean(arg, scope)?;
    let Some(ty) = operand_type(arg, scope) else {
        return Ok(());
    };
    if matches!(ty.as_str(), "bool" | "boolean") || category(&ty).is_none() {
        return Ok(());
    }
    let test = match B::try_from(t.booltesttype) {
        Ok(B::IsTrue) => "IS TRUE",
        Ok(B::IsNotTrue) => "IS NOT TRUE",
        Ok(B::IsFalse) => "IS FALSE",
        Ok(B::IsNotFalse) => "IS NOT FALSE",
        Ok(B::IsUnknown) => "IS UNKNOWN",
        Ok(B::IsNotUnknown) => "IS NOT UNKNOWN",
        _ => return Ok(()),
    };
    // `coerce_to_boolean` reports at `exprLocation`: an operator
    // expression's leftmost token (`(s || 'y') is true` at the `s`).
    let loc = crate::srf_place::leftmost(arg);
    if let Some(loc) = (loc >= 0).then_some(loc).or_else(|| expr_location(arg)) {
        set_error_location(loc);
    }
    Err(Error::DatatypeMismatch(format!(
        "argument of {test} must be type boolean, not type {}",
        display_type(&ty)
    )))
}

/// `x IN (SELECT y ...)` / `x op ANY (SELECT y ...)`: the operator between
/// `x` and the subquery's column must exist (`integer = text` is 42883,
/// where the lowering compared across types and matched nothing).
fn any_sublink_operator(sl: &pg_query::protobuf::SubLink, scope: &Scope) -> Result<()> {
    use pg_query::protobuf::SubLinkType;
    if !matches!(
        SubLinkType::try_from(sl.sub_link_type),
        Ok(SubLinkType::AnySublink | SubLinkType::AllSublink)
    ) {
        return Ok(());
    }
    let op = match sl.oper_name.last().and_then(|n| n.node.as_ref()) {
        Some(N::String(s)) => s.sval.clone(),
        None => "=".to_string(),
        _ => return Ok(()),
    };
    if !matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") {
        return Ok(());
    }
    let Some(l) = sl.testexpr.as_deref().and_then(|n| operand_type(n, scope)) else {
        return Ok(());
    };
    let mut as_scalar = sl.clone();
    as_scalar.sub_link_type = SubLinkType::ExprSublink as i32;
    as_scalar.testexpr = None;
    let node = pg_query::protobuf::Node {
        node: Some(N::SubLink(Box::new(as_scalar))),
    };
    let Some(r) = operand_type(&node, scope) else {
        return Ok(());
    };
    if matches!((category(&l), category(&r)), (Some(lc), Some(rc)) if lc != rc) {
        let op = if op == "!=" { "<>" } else { op.as_str() };
        return Err(mismatch(op, &l, &r, sl.location));
    }
    Ok(())
}

/// Do the untyped string literals among `args` read as the type the typed
/// ones share? The first literal that does not is its cast's error, at the
/// literal.
fn literals_fit(args: &[&pg_query::protobuf::Node], scope: &Scope) -> Result<()> {
    use pg_query::protobuf::a_const::Val;
    let untyped = |a: &pg_query::protobuf::Node| matches!(a.node.as_ref(), Some(N::AConst(c)) if c.isnull || matches!(c.val, Some(Val::Sval(_))));
    // The common type of the typed parts -- every one of them typed here,
    // or nothing is judged.
    let mut common: Option<String> = None;
    for a in args.iter().filter(|a| !untyped(a)) {
        let Some(t) = operand_type(a, scope) else {
            return Ok(());
        };
        common = Some(match common {
            None => t,
            Some(c) if c == t => c,
            Some(c) if category(&c) == Some("numeric") && category(&t) == Some("numeric") => {
                numeric_meet(&c, &t)
            }
            Some(_) => return Ok(()),
        });
    }
    let Some(first) = common.as_ref() else {
        return Ok(());
    };
    // Only the types whose input this check can judge exactly.
    if !matches!(
        first.as_str(),
        "int2" | "int4" | "int8" | "numeric" | "float4" | "float8" | "bool" | "date" | "uuid"
    ) {
        return Ok(());
    }
    for a in args {
        if let Some(N::AConst(c)) = a.node.as_ref() {
            if let Some(Val::Sval(s)) = c.val.as_ref() {
                if let Err(e) = crate::cast_value(bson::Bson::String(s.sval.clone()), first) {
                    set_error_location(c.location);
                    return Err(e);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::select_common;

    /// `select_common_type` over two typed inputs, as PostgreSQL 15.19
    /// answers it (each pair measured as a two-row VALUES column).
    #[test]
    fn select_common_follows_the_implicit_casts() {
        for (a, b, want) in [
            ("int4", "int8", Some("int8")),
            ("int8", "int4", Some("int8")),
            ("numeric", "float4", Some("float4")),
            ("int2", "float4", Some("float4")),
            ("float4", "float8", Some("float8")),
            ("varchar", "bpchar", Some("varchar")),
            ("bpchar", "varchar", Some("bpchar")),
            ("varchar", "name", Some("name")),
            ("bpchar", "name", Some("name")),
            ("name", "varchar", Some("name")),
            ("name", "text", Some("name")),
            ("text", "name", Some("text")),
            ("text", "varchar", Some("text")),
            ("varchar", "text", Some("varchar")),
            ("bpchar", "text", Some("bpchar")),
            ("char", "text", None),
            ("text", "char", None),
            ("int4", "text", None),
            ("bool", "int4", None),
        ] {
            assert_eq!(select_common(a, b).as_deref(), want, "{a} with {b}");
        }
    }
}
