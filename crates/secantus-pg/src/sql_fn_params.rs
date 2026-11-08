//! A SQL function body's references to its parameters BY NAME (RP-461),
//! resolved as `functions.c`'s `sql_fn_post_column_ref` resolves them: a
//! column reference is a column first, and a parameter only when no column
//! of that name is in scope.
//!
//! PostgreSQL's parse analysis looks a `ColumnRef` up in its query level's
//! namespace and then in each enclosing level's (`colNameToVar`), then as a
//! whole-row reference to a relation of that name, and only when all of
//! that finds nothing calls the post-columnref hook, which a SQL function
//! sets to bind a parameter. So `gb(a int)` with the body `select a from fe`
//! reads fe's column `a`, and nothing but a `ColumnRef` is ever a
//! parameter: the alias in `select 1 as p` is not one.
//!
//! This walk reproduces that lookup over the raw parse tree, statement by
//! statement, with the namespace of each query level built as
//! `transformFromClause` builds it: a FROM item's columns from the catalog
//! (a table's), from its column aliases or target list (a CTE's, a
//! subquery's), or from the function's result (`pg_proc`'s OUT parameters,
//! or a scalar function's one column). A reference it finds no column for
//! and that names a parameter is rewritten to `$n` for the planner. Where
//! a relation's columns cannot be known here (a table function returning
//! `record` with no column list, a name that is no table) it is taken to
//! have the column, so the reference is left to the planner: it then
//! answers 42703 rather than reading the parameter in its place.
//!
//! The hook also reads a field of a composite parameter (RP-462): `p.f`,
//! or `fname.p.f`, is the field `f` of the parameter `p` when no relation
//! in scope known as `p` (for `fname.p.f`, a table `fname.p`) has a column
//! `f`, and `f` is a field of the parameter's type; it is rewritten to
//! `($n::type).f`.
//!
//! The raw tree is walked as the JSON its serde derive writes (as
//! `nodetostring::cte_walk` walks it), so every field of every node is
//! reached; the nodes that open a namespace are followed by name.

use serde_json::{Map, Value};

/// What a function called in FROM returns, as columns.
pub(crate) enum FnColumns {
    /// One column, named by the call's alias or the function's name.
    Scalar,
    /// Named columns: OUT parameters or a composite type's attributes.
    Named(Vec<String>),
    /// Not known here (a `record` without OUT parameters).
    Unknown,
}

/// The catalog facts the walk needs.
pub(crate) struct Lookups<'a> {
    /// `Some(has)` for a stored table or view named `key` (`schema.name`,
    /// or the bare name in `public`), `None` for a name that is not one.
    pub table_has_column: &'a dyn Fn(&str, &str) -> Option<bool>,
    /// The columns of the function `name` returns, over all its overloads.
    pub function_columns: &'a dyn Fn(&str) -> FnColumns,
    /// Whether the type `ty` is a row type with the field `field`.
    pub type_has_field: &'a dyn Fn(&str, &str) -> bool,
}

/// What a reference to a parameter is rewritten to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Bound {
    /// The parameter itself: `$n::type`.
    Value,
    /// A field of it: `($n::type).` before the field's name.
    Field,
}

#[derive(Clone, Debug)]
enum Cols {
    Table(String),
    Names(Vec<String>),
    Union(Vec<Cols>),
    Unknown,
}

#[derive(Clone, Debug)]
struct Item {
    /// The name a qualified reference finds it by; `None` for an
    /// unaliased subquery or join, which no qualifier names.
    refname: Option<String>,
    cols: Cols,
}

struct Binder<'a> {
    fname: &'a str,
    names: &'a [String],
    types: &'a [String],
    look: &'a Lookups<'a>,
    /// The namespaces of the enclosing query levels, innermost last.
    levels: Vec<Vec<Item>>,
    /// The CTE names in scope, innermost last, with their output columns.
    ctes: Vec<(String, Cols)>,
    /// `(location, names, parameter, what)` of each reference to bind.
    binds: Vec<(usize, usize, usize, Bound)>,
}

fn tagged(v: &Value) -> Option<(&str, &Map<String, Value>)> {
    let o = v.as_object()?;
    if o.len() != 1 {
        return None;
    }
    let (k, inner) = o.get("node")?.as_object()?.iter().next()?;
    Some((k.as_str(), inner.as_object()?))
}

fn field_str<'a>(o: &'a Map<String, Value>, k: &str) -> &'a str {
    o.get(k).and_then(Value::as_str).unwrap_or("")
}

fn list<'a>(o: &'a Map<String, Value>, k: &str) -> &'a [Value] {
    o.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// The `String` nodes of a list (an alias's column names, a name list).
fn strings(items: &[Value]) -> Vec<String> {
    items
        .iter()
        .filter_map(|v| match tagged(v) {
            Some(("String", s)) => Some(field_str(s, "sval").to_string()),
            _ => None,
        })
        .collect()
}

/// A ColumnRef's fields: `Some(name)` for each name, `None` for a `*`.
fn column_ref_fields(o: &Map<String, Value>) -> Vec<Option<String>> {
    list(o, "fields")
        .iter()
        .map(|f| match tagged(f) {
            Some(("String", s)) => Some(field_str(s, "sval").to_string()),
            _ => None,
        })
        .collect()
}

/// A bare `ColumnRef` naming one column, as a sort or group item may be.
fn bare_name(v: &Value) -> Option<String> {
    let (k, o) = tagged(v)?;
    if k != "ColumnRef" {
        return None;
    }
    match column_ref_fields(o).as_slice() {
        [Some(n)] => Some(n.clone()),
        _ => None,
    }
}

/// `FigureColnameInternal`: the name a target list entry is given, or
/// `None` where PostgreSQL gives it `?column?`.
fn figure_colname(v: &Value) -> Option<String> {
    let (k, o) = tagged(v)?;
    match k {
        "ColumnRef" => column_ref_fields(o).into_iter().last().flatten(),
        "A_Indirection" => strings(list(o, "indirection"))
            .pop()
            .or_else(|| figure_colname(o.get("arg")?)),
        "FuncCall" => strings(list(o, "funcname")).pop(),
        // AEXPR_NULLIF (the protobuf's numbering is PostgreSQL's plus one).
        "A_Expr" => (o.get("kind").and_then(Value::as_i64) == Some(6)).then(|| "nullif".into()),
        "TypeCast" => figure_colname(o.get("arg")?).or_else(|| {
            let t = o.get("type_name")?.as_object()?;
            strings(list(t, "names")).pop()
        }),
        "CollateClause" => figure_colname(o.get("arg")?),
        "GroupingFunc" => Some("grouping".into()),
        "SubLink" => {
            // EXISTS_SUBLINK, ARRAY_SUBLINK, EXPR_SUBLINK, numbered as above.
            match o.get("sub_link_type").and_then(Value::as_i64) {
                Some(1) => Some("exists".into()),
                Some(7) => Some("array".into()),
                Some(5) => {
                    let (_, s) = tagged(o.get("subselect")?)?;
                    let first = list(s, "target_list").first()?;
                    let (_, rt) = tagged(first)?;
                    let name = field_str(rt, "name");
                    (!name.is_empty()).then(|| name.to_string())
                }
                _ => None,
            }
        }
        "CaseExpr" => Some("case".into()),
        "A_ArrayExpr" => Some("array".into()),
        "RowExpr" => Some("row".into()),
        "CoalesceExpr" => Some("coalesce".into()),
        _ => None,
    }
}

/// A query's output column names: `None` where a `*` leaves them to the
/// planner. `v` is a tagged node or an untagged SelectStmt.
fn output_names(v: &Value) -> Option<Vec<String>> {
    let (kind, o) = match tagged(v) {
        Some(t) => t,
        None => ("SelectStmt", v.as_object()?),
    };
    let targets = match kind {
        "SelectStmt" => {
            if let Some(l) = o.get("larg").filter(|l| !l.is_null()) {
                return output_names(l);
            }
            if let Some(row) = list(o, "values_lists").first() {
                let n = tagged(row).map_or(0, |(_, l)| list(l, "items").len());
                return Some((1..=n).map(|i| format!("column{i}")).collect());
            }
            list(o, "target_list")
        }
        "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt" => list(o, "returning_list"),
        _ => return None,
    };
    targets
        .iter()
        .map(|t| {
            let (_, rt) = tagged(t)?;
            let name = field_str(rt, "name");
            if !name.is_empty() {
                return Some(name.to_string());
            }
            let val = rt.get("val")?;
            if let Some(("ColumnRef", c)) = tagged(val) {
                if column_ref_fields(c).last().is_some_and(Option::is_none) {
                    return None;
                }
            }
            Some(figure_colname(val).unwrap_or_else(|| "?column?".into()))
        })
        .collect()
}

impl Binder<'_> {
    fn has(&self, cols: &Cols, column: &str) -> bool {
        match cols {
            Cols::Table(key) => (self.look.table_has_column)(key, column).unwrap_or(true),
            Cols::Names(n) => n.iter().any(|c| c == column),
            Cols::Union(all) => all.iter().any(|c| self.has(c, column)),
            Cols::Unknown => true,
        }
    }

    fn param(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|n| !n.is_empty() && n == name)
    }

    fn column_in_scope(&self, column: &str) -> bool {
        self.levels
            .iter()
            .flatten()
            .any(|item| self.has(&item.cols, column))
    }

    fn relation_in_scope(&self, refname: &str) -> bool {
        self.levels
            .iter()
            .flatten()
            .any(|item| item.refname.as_deref() == Some(refname))
    }

    /// `transformColumnRef`, then `sql_fn_post_column_ref`.
    fn column_ref(&mut self, o: &Map<String, Value>) {
        let location = o.get("location").and_then(Value::as_i64).unwrap_or(-1);
        if location < 0 {
            return;
        }
        match column_ref_fields(o).as_slice() {
            [Some(n)] => {
                let Some(pos) = self.param(n) else {
                    return;
                };
                if self.column_in_scope(n) || self.relation_in_scope(n) {
                    return;
                }
                self.binds.push((location as usize, 1, pos, Bound::Value));
            }
            [Some(q), Some(n)] => {
                // A relation known as `q` with a column `n` comes first.
                let column = self
                    .levels
                    .iter()
                    .flatten()
                    .any(|i| i.refname.as_deref() == Some(q) && self.has(&i.cols, n));
                // `fname.p`.
                if let Some(pos) = (!self.fname.is_empty() && q == self.fname)
                    .then(|| self.param(n))
                    .flatten()
                {
                    if !column {
                        self.binds.push((location as usize, 2, pos, Bound::Value));
                    }
                    return;
                }
                // `p.f`, a field of the composite parameter `p`.
                if let Some(pos) = self.param(q) {
                    if !column && self.field_of(pos, n) {
                        self.binds.push((location as usize, 2, pos, Bound::Field));
                    }
                }
            }
            // `fname.p.f`, unless a table `fname.p` read by its name has a
            // column `f`.
            [Some(q), Some(p), Some(f)] if !self.fname.is_empty() && q == self.fname => {
                let Some(pos) = self.param(p) else {
                    return;
                };
                let key = format!("{q}.{p}");
                let column = self.levels.iter().flatten().any(|i| {
                    i.refname.as_deref() == Some(p)
                        && matches!(&i.cols, Cols::Table(k) if *k == key)
                        && self.has(&i.cols, f)
                });
                if !column && self.field_of(pos, f) {
                    self.binds.push((location as usize, 3, pos, Bound::Field));
                }
            }
            _ => {}
        }
    }

    fn field_of(&self, pos: usize, field: &str) -> bool {
        self.types
            .get(pos)
            .is_some_and(|t| (self.look.type_has_field)(t, field))
    }

    /// `raw_expression_tree_walker`, with the nodes that open a namespace
    /// followed by name.
    fn walk(&mut self, v: &Value) {
        match v {
            Value::Array(a) => a.iter().for_each(|x| self.walk(x)),
            Value::Object(o) => match tagged(v) {
                Some(("ColumnRef", c)) => self.column_ref(c),
                Some(("SelectStmt", s)) => {
                    self.select(s);
                }
                Some((k @ ("InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt"), s)) => {
                    self.dml(k, s);
                }
                _ => o.values().for_each(|x| self.walk(x)),
            },
            _ => {}
        }
    }

    /// A query in a FROM item or a CTE: walked, and its output columns.
    fn query(&mut self, v: &Value) -> Cols {
        match tagged(v) {
            Some(("SelectStmt", s)) => self.select(s),
            Some((k @ ("InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt"), s)) => {
                self.dml(k, s)
            }
            _ => {
                self.walk(v);
                Cols::Unknown
            }
        }
    }

    /// A target list's output columns over the namespace `items`, a `*`
    /// expanded to the columns of the items it names.
    fn target_cols(&self, targets: &[Value], items: &[Item]) -> Cols {
        let mut names = Vec::new();
        let mut more = Vec::new();
        for t in targets {
            let Some((_, rt)) = tagged(t) else {
                continue;
            };
            let name = field_str(rt, "name");
            if !name.is_empty() {
                names.push(name.to_string());
                continue;
            }
            let Some(val) = rt.get("val") else {
                continue;
            };
            if let Some(("ColumnRef", c)) = tagged(val) {
                match column_ref_fields(c).as_slice() {
                    [None] => {
                        more.extend(items.iter().map(|i| i.cols.clone()));
                        continue;
                    }
                    [Some(q), None] => {
                        more.extend(
                            items
                                .iter()
                                .filter(|i| i.refname.as_deref() == Some(q))
                                .map(|i| i.cols.clone()),
                        );
                        continue;
                    }
                    f if f.last().is_some_and(Option::is_none) => {
                        more.push(Cols::Unknown);
                        continue;
                    }
                    _ => {}
                }
            }
            names.push(figure_colname(val).unwrap_or_else(|| "?column?".into()));
        }
        more.insert(0, Cols::Names(names));
        Cols::Union(more)
    }

    fn walk_field(&mut self, o: &Map<String, Value>, k: &str) {
        if let Some(v) = o.get(k) {
            self.walk(v);
        }
    }

    /// A WITH clause: its CTEs come into scope (all at once when it is
    /// RECURSIVE, else each after its own query), and each query is walked
    /// at the level the clause is written at. Returns how many it pushed.
    fn with_clause(&mut self, o: &Map<String, Value>) -> usize {
        let Some(w) = o.get("with_clause").and_then(Value::as_object) else {
            return 0;
        };
        let recursive = w.get("recursive").and_then(Value::as_bool) == Some(true);
        let ctes: Vec<(String, Vec<String>, Value)> = list(w, "ctes")
            .iter()
            .filter_map(|c| {
                let (_, c) = tagged(c)?;
                let query = c.get("ctequery").cloned().unwrap_or(Value::Null);
                let aliased = strings(list(c, "aliascolnames"));
                Some((field_str(c, "ctename").to_string(), aliased, query))
            })
            .collect();
        let named = |aliased: &[String], cols: Cols| {
            if aliased.is_empty() {
                cols
            } else {
                Cols::Union(vec![Cols::Names(aliased.to_vec()), cols])
            }
        };
        if recursive {
            // Each in scope of all of them, by its non-recursive term's
            // names until its query is walked.
            let base = self.ctes.len();
            for (name, aliased, q) in &ctes {
                let first = output_names(q).map_or(Cols::Unknown, Cols::Names);
                self.ctes.push((name.clone(), named(aliased, first)));
            }
            for (i, (_, aliased, q)) in ctes.iter().enumerate() {
                let cols = self.query(q);
                self.ctes[base + i].1 = named(aliased, cols);
            }
        } else {
            for (name, aliased, q) in &ctes {
                let cols = self.query(q);
                self.ctes.push((name.clone(), named(aliased, cols)));
            }
        }
        ctes.len()
    }

    /// A SELECT, and its output columns.
    fn select(&mut self, o: &Map<String, Value>) -> Cols {
        let pushed = self.with_clause(o);
        let out;
        // SetOperation: the protobuf's numbering is PostgreSQL's plus one.
        if o.get("op").and_then(Value::as_i64).unwrap_or(1) > 1 {
            // A set operation's columns are named by its first arm.
            let mut arms = Vec::new();
            for arm in ["larg", "rarg"] {
                if let Some(s) = o.get(arm).and_then(Value::as_object) {
                    arms.push(self.select(s));
                }
            }
            out = arms.into_iter().next().unwrap_or(Cols::Unknown);
            // ORDER BY / LIMIT of a set operation see its output columns.
            self.levels.push(vec![Item {
                refname: None,
                cols: out.clone(),
            }]);
            for k in ["sort_clause", "limit_offset", "limit_count"] {
                self.walk_field(o, k);
            }
            self.levels.pop();
        } else {
            let mut items = Vec::new();
            for from in list(o, "from_clause") {
                let visible = items.clone();
                self.range_item(from, &visible, &mut items);
            }
            out = match list(o, "values_lists").first() {
                Some(row) => {
                    let n = tagged(row).map_or(0, |(_, l)| list(l, "items").len());
                    Cols::Names((1..=n).map(|i| format!("column{i}")).collect())
                }
                None => self.target_cols(list(o, "target_list"), &items),
            };
            self.levels.push(items);
            let aliases: Vec<String> = list(o, "target_list")
                .iter()
                .filter_map(|t| {
                    let (_, rt) = tagged(t)?;
                    let n = field_str(rt, "name");
                    (!n.is_empty()).then(|| n.to_string())
                })
                .collect();
            for k in [
                "distinct_clause",
                "target_list",
                "where_clause",
                "having_clause",
                "window_clause",
                "values_lists",
                "limit_offset",
                "limit_count",
                "locking_clause",
            ] {
                self.walk_field(o, k);
            }
            // `findTargetlistEntrySQL92`: a bare name in GROUP BY that no
            // input column has, or any bare name in ORDER BY, that an
            // output column is called is that output column.
            for g in list(o, "group_clause") {
                match bare_name(g) {
                    Some(n) if !self.column_in_scope(&n) && aliases.contains(&n) => {}
                    _ => self.walk(g),
                }
            }
            for s in list(o, "sort_clause") {
                let by = tagged(s).and_then(|(_, b)| b.get("node"));
                match by.and_then(bare_name) {
                    Some(n) if aliases.contains(&n) => {}
                    _ => self.walk(s),
                }
            }
            self.levels.pop();
        }
        self.ctes.truncate(self.ctes.len() - pushed);
        out
    }

    /// A relation named in FROM or as a statement's target.
    fn range_var(&self, o: &Map<String, Value>) -> Item {
        let schema = field_str(o, "schemaname");
        let relname = field_str(o, "relname");
        let alias = o.get("alias").and_then(Value::as_object);
        let aliasname = alias.map(|a| field_str(a, "aliasname")).unwrap_or("");
        let base = match self
            .ctes
            .iter()
            .rev()
            .find(|(n, _)| schema.is_empty() && n == relname)
        {
            Some((_, cols)) => cols.clone(),
            None => Cols::Table(if schema.is_empty() || schema == "public" {
                relname.to_string()
            } else {
                format!("{schema}.{relname}")
            }),
        };
        Item {
            refname: Some(
                if aliasname.is_empty() {
                    relname
                } else {
                    aliasname
                }
                .to_string(),
            ),
            cols: renamed(alias, base),
        }
    }

    /// One FROM item, `transformFromClauseItem`: its expressions are walked
    /// at the levels it may see (a LATERAL item, and a function, also the
    /// items before it, `visible`), and what it adds to the namespace is
    /// pushed on `items`.
    fn range_item(&mut self, v: &Value, visible: &[Item], items: &mut Vec<Item>) {
        let Some((kind, o)) = tagged(v) else {
            return;
        };
        match kind {
            "RangeVar" => items.push(self.range_var(o)),
            "RangeSubselect" => {
                let lateral = o.get("lateral").and_then(Value::as_bool) == Some(true);
                let query = o.get("subquery").cloned().unwrap_or(Value::Null);
                if lateral {
                    self.levels.push(visible.to_vec());
                }
                let base = self.query(&query);
                if lateral {
                    self.levels.pop();
                }
                let alias = o.get("alias").and_then(Value::as_object);
                items.push(Item {
                    refname: alias
                        .map(|a| field_str(a, "aliasname").to_string())
                        .filter(|a| !a.is_empty()),
                    cols: renamed(alias, base),
                });
            }
            "RangeFunction" => {
                // A function in FROM may read the items before it.
                self.levels.push(visible.to_vec());
                self.walk_field(o, "functions");
                self.levels.pop();
                items.push(self.range_function(o));
            }
            "JoinExpr" => {
                let mut sides = Vec::new();
                for arm in ["larg", "rarg"] {
                    if let Some(a) = o.get(arm) {
                        let mut seen = visible.to_vec();
                        seen.extend(sides.iter().cloned());
                        self.range_item(a, &seen, &mut sides);
                    }
                }
                // ON sees the join's own sides.
                self.levels.push(sides.clone());
                self.walk_field(o, "quals");
                self.levels.pop();
                let alias = o.get("alias").and_then(Value::as_object);
                let aliasname = alias.map(|a| field_str(a, "aliasname")).unwrap_or("");
                if let Some(u) = o.get("join_using_alias").and_then(Value::as_object) {
                    items.push(Item {
                        refname: Some(field_str(u, "aliasname").to_string()),
                        cols: Cols::Names(strings(list(o, "using_clause"))),
                    });
                }
                if aliasname.is_empty() {
                    items.extend(sides);
                } else {
                    // An aliased join hides its sides.
                    let base = Cols::Union(sides.into_iter().map(|s| s.cols).collect());
                    items.push(Item {
                        refname: Some(aliasname.to_string()),
                        cols: renamed(alias, base),
                    });
                }
            }
            "RangeTableSample" => {
                self.walk_field(o, "args");
                self.walk_field(o, "repeatable");
                if let Some(r) = o.get("relation") {
                    self.range_item(r, visible, items);
                }
            }
            _ => {
                self.walk(v);
                items.push(Item {
                    refname: None,
                    cols: Cols::Unknown,
                });
            }
        }
    }

    fn range_function(&self, o: &Map<String, Value>) -> Item {
        let alias = o.get("alias").and_then(Value::as_object);
        let aliasname = alias.map(|a| field_str(a, "aliasname")).unwrap_or("");
        // Each `functions` entry is a two-item List: the call, its column
        // definition list.
        let calls: Vec<(String, Vec<String>)> = list(o, "functions")
            .iter()
            .filter_map(|f| {
                let (_, l) = tagged(f)?;
                let items = list(l, "items");
                let (k, call) = tagged(items.first()?)?;
                let name = if k == "FuncCall" {
                    strings(list(call, "funcname")).pop().unwrap_or_default()
                } else {
                    String::new()
                };
                let coldefs = items
                    .get(1)
                    .and_then(tagged)
                    .map(|(_, l)| {
                        list(l, "items")
                            .iter()
                            .filter_map(|c| {
                                let (_, d) = tagged(c)?;
                                Some(field_str(d, "colname").to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some((name, coldefs))
            })
            .collect();
        let mut coldefs: Vec<String> = list(o, "coldeflist")
            .iter()
            .filter_map(|c| {
                let (_, d) = tagged(c)?;
                Some(field_str(d, "colname").to_string())
            })
            .collect();
        let mut base = Vec::new();
        for (name, defs) in &calls {
            coldefs.extend(defs.iter().cloned());
            base.push(if !coldefs.is_empty() {
                Cols::Names(coldefs.clone())
            } else if name.is_empty() {
                Cols::Unknown
            } else {
                match (self.look.function_columns)(name) {
                    // A lone scalar function's column is named as the
                    // item is (`addRangeTableEntryForFunction`); in ROWS
                    // FROM, by the function.
                    FnColumns::Scalar => {
                        Cols::Names(vec![if calls.len() == 1 && !aliasname.is_empty() {
                            aliasname.to_string()
                        } else {
                            name.clone()
                        }])
                    }
                    FnColumns::Named(n) => Cols::Names(n),
                    FnColumns::Unknown => Cols::Unknown,
                }
            });
        }
        if o.get("ordinality").and_then(Value::as_bool) == Some(true) {
            base.push(Cols::Names(vec!["ordinality".into()]));
        }
        let refname = if !aliasname.is_empty() {
            Some(aliasname.to_string())
        } else if calls.len() == 1 && !calls[0].0.is_empty() {
            Some(calls[0].0.clone())
        } else {
            None
        };
        Item {
            refname,
            cols: renamed(alias, Cols::Union(base)),
        }
    }

    /// INSERT, UPDATE, DELETE and MERGE: the target is in scope for what
    /// `transformInsertStmt` and the others analyse against it, never for
    /// an INSERT's VALUES or SELECT.
    fn dml(&mut self, kind: &str, o: &Map<String, Value>) -> Cols {
        let pushed = self.with_clause(o);
        let target = o
            .get("relation")
            .and_then(Value::as_object)
            .map(|r| self.range_var(r));
        let mut level: Vec<Item> = target.iter().cloned().collect();
        match kind {
            "InsertStmt" => {
                self.walk_field(o, "select_stmt");
                if let Some(c) = o.get("on_conflict_clause").and_then(Value::as_object) {
                    let mut conflict = level.clone();
                    if let Some(t) = &target {
                        conflict.push(Item {
                            refname: Some("excluded".into()),
                            cols: t.cols.clone(),
                        });
                    }
                    self.levels.push(conflict);
                    self.walk_field(c, "infer");
                    self.walk_field(c, "target_list");
                    self.walk_field(c, "where_clause");
                    self.levels.pop();
                }
            }
            "UpdateStmt" | "DeleteStmt" => {
                let from = if kind == "UpdateStmt" {
                    "from_clause"
                } else {
                    "using_clause"
                };
                for f in list(o, from) {
                    let visible = level.clone();
                    self.range_item(f, &visible, &mut level);
                }
                self.levels.push(level.clone());
                self.walk_field(o, "target_list");
                self.walk_field(o, "where_clause");
                self.levels.pop();
            }
            _ => {
                if let Some(s) = o.get("source_relation") {
                    let visible = level.clone();
                    self.range_item(s, &visible, &mut level);
                }
                self.levels.push(level.clone());
                self.walk_field(o, "join_condition");
                self.walk_field(o, "merge_when_clauses");
                self.levels.pop();
            }
        }
        let returning: Vec<Item> = target.into_iter().collect();
        let out = self.target_cols(list(o, "returning_list"), &returning);
        self.levels.push(returning);
        self.walk_field(o, "returning_list");
        self.levels.pop();
        self.ctes.truncate(self.ctes.len() - pushed);
        out
    }
}

/// An alias's column names rename the relation's first columns.
fn renamed(alias: Option<&Map<String, Value>>, base: Cols) -> Cols {
    let names = alias
        .map(|a| strings(list(a, "colnames")))
        .unwrap_or_default();
    if names.is_empty() {
        base
    } else {
        Cols::Union(vec![Cols::Names(names), base])
    }
}

/// Whether PostgreSQL parse-analyses this statement kind in a SQL function,
/// with the parameter hooks set; a utility statement it does not (its
/// expressions are not resolved against the function's parameters).
fn analysed(kind: &str) -> bool {
    matches!(
        kind,
        "SelectStmt"
            | "InsertStmt"
            | "UpdateStmt"
            | "DeleteStmt"
            | "MergeStmt"
            | "CallStmt"
            | "CreateTableAsStmt"
            | "ExplainStmt"
            | "DeclareCursorStmt"
    )
}

/// Whether `fmgr_sql_validator` parse-analyses this statement of a body at
/// CREATE: one `transformStmt` transforms (a utility statement, which it
/// wraps, is analysed only when it runs): a query or a write.
pub(crate) fn analysed_at_create(sql: &str) -> bool {
    let Ok(tree) = pg_query::parse(sql) else {
        return false;
    };
    tree.protobuf.stmts.iter().all(|raw| {
        serde_json::to_value(&raw.stmt).is_ok_and(|v| {
            tagged(&v).is_some_and(|(k, _)| {
                matches!(
                    k,
                    "SelectStmt" | "InsertStmt" | "UpdateStmt" | "DeleteStmt" | "MergeStmt"
                )
            })
        })
    }) && !tree.protobuf.stmts.is_empty()
}

/// One statement of a SQL function's body with each reference to a
/// parameter by name -- `p`, or `fname.p` -- rewritten to `$n::type`.
/// `names` are the parameters' names, `types` their types.
pub(crate) fn bind_parameter_names(
    sql: &str,
    fname: &str,
    names: &[String],
    types: &[String],
    look: &Lookups<'_>,
) -> String {
    bind_parameter_names_mapped(sql, fname, names, types, look).0
}

/// One replacement [`bind_parameter_names_mapped`] made: the bytes
/// `original` of the statement as written became the bytes `bound` of the
/// text it returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BoundSpan {
    pub(crate) original: std::ops::Range<usize>,
    pub(crate) bound: std::ops::Range<usize>,
}

/// The byte offset in the statement as written of byte `at` of the text
/// [`bind_parameter_names_mapped`] returned: inside a replacement, the
/// reference it replaced.
pub(crate) fn original_offset(spans: &[BoundSpan], at: usize) -> usize {
    let mut shift: isize = 0;
    for s in spans {
        if at < s.bound.start {
            break;
        }
        if at < s.bound.end {
            return s.original.start;
        }
        shift = s.bound.end as isize - s.original.end as isize;
    }
    (at as isize - shift).max(0) as usize
}

/// [`bind_parameter_names`], with the spans it replaced, so a position in
/// the text it returned can be put back into the statement as written.
pub(crate) fn bind_parameter_names_mapped(
    sql: &str,
    fname: &str,
    names: &[String],
    types: &[String],
    look: &Lookups<'_>,
) -> (String, Vec<BoundSpan>) {
    if names.iter().all(String::is_empty) {
        return (sql.to_string(), Vec::new());
    }
    let (Ok(tree), Ok(scan)) = (pg_query::parse(sql), pg_query::scan(sql)) else {
        return (sql.to_string(), Vec::new());
    };
    let mut b = Binder {
        fname,
        names,
        types,
        look,
        levels: Vec::new(),
        ctes: Vec::new(),
        binds: Vec::new(),
    };
    for raw in &tree.protobuf.stmts {
        let Ok(v) = serde_json::to_value(&raw.stmt) else {
            return (sql.to_string(), Vec::new());
        };
        if tagged(&v).is_some_and(|(k, _)| analysed(k)) {
            b.walk(&v);
        }
    }
    let mut binds = b.binds;
    binds.sort_unstable();
    binds.dedup_by_key(|b| b.0);
    let tokens = scan.tokens;
    let mut out = String::new();
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    let text = |i: usize| {
        tokens
            .get(i)
            .and_then(|t| sql.get(t.start as usize..t.end as usize))
    };
    for (location, names, pos, what) in binds {
        // The reference's tokens: its names and the dots between them. A
        // parameter replaces them all, a field's parameter all but the
        // field's name.
        let Some(first) = tokens.iter().position(|t| t.start as usize == location) else {
            continue;
        };
        let last = match what {
            Bound::Value => first + 2 * (names - 1),
            Bound::Field => first + 2 * names - 3,
        };
        let dots_ok = (first + 1..=last).step_by(2).all(|i| text(i) == Some("."));
        let Some(end) = tokens.get(last).filter(|_| dots_ok && location >= cursor) else {
            continue;
        };
        let ty = types.get(pos).map_or("text", String::as_str);
        // A subscript after the reference (`v[1]`) would read as part of the
        // cast's type name (`$1::int4[][1]`, an array type with a bound), so
        // the cast is parenthesised there: `($1::int4[])[1]` subscripts it.
        let subscripted = text(last + 1) == Some("[");
        out.push_str(&sql[cursor..location]);
        let start = out.len();
        out.push_str(&match what {
            Bound::Value if subscripted => format!("(${}::{ty})", pos + 1),
            Bound::Value => format!("${}::{ty}", pos + 1),
            Bound::Field => format!("(${}::{ty}).", pos + 1),
        });
        cursor = end.end as usize;
        spans.push(BoundSpan {
            original: location..cursor,
            bound: start..out.len(),
        });
    }
    out.push_str(&sql[cursor..]);
    (out, spans)
}

/// `match_prosrc_to_query` (`pg_proc.c`): where in `query`, counted in
/// characters from 1, character `cursor` (from 1) of a routine's source
/// `prosrc` is, found as the one dollar-quoted or quoted literal holding
/// it; `None` when there is no such literal, or more than one.
pub(crate) fn match_prosrc_to_query(prosrc: &str, query: &str, cursor: usize) -> Option<usize> {
    let q = query.as_bytes();
    let n = prosrc.len();
    let mut found = None;
    for (at, &b) in q.iter().enumerate() {
        if at + n >= q.len() {
            break;
        }
        let matched = if b == b'$'
            && query.get(at + 1..at + 1 + n) == Some(prosrc)
            && q[at + 1 + n] == b'$'
        {
            Some(query[..at + 1].chars().count() + cursor)
        } else if b == b'\'' {
            match_prosrc_to_literal(prosrc, &query[at + 1..], cursor)
                .map(|c| query[..at + 1].chars().count() + c)
        } else {
            None
        };
        if let Some(p) = matched {
            if found.is_some() {
                return None;
            }
            found = Some(p);
        }
    }
    found
}

/// `match_prosrc_to_literal`: whether `literal` (just past its opening
/// quote) spells `prosrc`, a backslash or a doubled quote before each
/// character it escapes, and ends there; the cursor moved past each
/// escape that precedes it.
fn match_prosrc_to_literal(prosrc: &str, literal: &str, cursor: usize) -> Option<usize> {
    let lit = literal.as_bytes();
    let mut i = 0usize;
    let mut left = cursor as isize;
    let mut moved = cursor;
    for ch in prosrc.chars() {
        left -= 1;
        match lit.get(i) {
            Some(b'\\') => {
                i += 1;
                if left > 0 {
                    moved += 1;
                }
            }
            Some(b'\'') => {
                if lit.get(i + 1) != Some(&b'\'') {
                    return None;
                }
                i += 1;
                if left > 0 {
                    moved += 1;
                }
            }
            _ => {}
        }
        let len = ch.len_utf8();
        if literal.get(i..i + len)? != ch.encode_utf8(&mut [0; 4]) {
            return None;
        }
        i += len;
    }
    (lit.get(i) == Some(&b'\'') && lit.get(i + 1) != Some(&b'\'')).then_some(moved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prosrc_is_found_in_its_dollar_quoted_or_quoted_literal() {
        // PostgreSQL 15.19's positions for `select nosuch` / `select 'x' ||
        // nosuch` bodies (RP-465).
        let q = "create function gx(p int) returns int language sql as $$ select nosuch $$";
        assert_eq!(match_prosrc_to_query(" select nosuch ", q, 9), Some(65));
        let q = "create function gx(p int) returns int language sql as $f$select nosuch$f$";
        assert_eq!(match_prosrc_to_query("select nosuch", q, 8), Some(65));
        let q = "create function gx(p int) returns int language sql as 'select ''x'' || nosuch'";
        assert_eq!(
            match_prosrc_to_query("select 'x' || nosuch", q, 15),
            Some(72)
        );
        // Two literals spelling it: no place (`match_prosrc_to_query`'s
        // "multiple matches, fail").
        let q = "select $$ x $$, $$ x $$";
        assert_eq!(match_prosrc_to_query(" x ", q, 2), None);
        assert_eq!(match_prosrc_to_query("select 1", "select 2", 1), None);
    }

    #[test]
    fn a_position_in_the_bound_text_maps_back() {
        // `select p + q` with `p` bound: `select $1::integer + q`.
        let spans = vec![BoundSpan {
            original: 7..8,
            bound: 7..18,
        }];
        assert_eq!(original_offset(&spans, 0), 0);
        assert_eq!(original_offset(&spans, 10), 7);
        assert_eq!(original_offset(&spans, 21), 11);
    }

    fn bind(sql: &str, params: &[&str]) -> String {
        let names: Vec<String> = params.iter().map(|p| p.to_string()).collect();
        let types = vec!["int4".to_string(); names.len()];
        bind_typed(sql, &names, &types)
    }

    /// A catalog of tables `fe(a, b)`, `fs(a, c)`, `f(p)`, `r(a)` and
    /// `s.r(a)`, and the functions `generate_series` and `jsonb_each`; a
    /// row type's fields are its table's columns.
    fn bind_typed(sql: &str, names: &[String], types: &[String]) -> String {
        let table = |t: &str, c: &str| match t {
            "fe" => Some(["a", "b"].contains(&c)),
            "fs" => Some(["a", "c"].contains(&c)),
            "f" => Some(c == "p"),
            "r" | "s.r" => Some(c == "a"),
            _ => None,
        };
        let functions = |f: &str| match f {
            "generate_series" => FnColumns::Scalar,
            "jsonb_each" => FnColumns::Named(vec!["key".into(), "value".into()]),
            _ => FnColumns::Unknown,
        };
        let fields = |t: &str, f: &str| table(t, f).unwrap_or(false);
        let look = Lookups {
            table_has_column: &table,
            function_columns: &functions,
            type_has_field: &fields,
        };
        bind_parameter_names(sql, "f", names, types, &look)
    }

    /// `f(r fe, p int)` (RP-462).
    fn fields(sql: &str) -> String {
        bind_typed(
            sql,
            &["r".into(), "p".into()],
            &["fe".into(), "int4".into()],
        )
    }

    #[test]
    fn an_array_parameters_subscript_subscripts_the_cast() {
        // h1(v int[]) answered 4, 5 and 7 on PostgreSQL 15.19 (RP-502).
        let arr = |sql: &str| bind_typed(sql, &["v".into()], &["int4[]".into()]);
        assert_eq!(arr("select v[1]"), "select ($1::int4[])[1]");
        assert_eq!(arr("select v[1] + 1"), "select ($1::int4[])[1] + 1");
        assert_eq!(
            arr("select v[2] + array_length(v, 1)"),
            "select ($1::int4[])[2] + array_length($1::int4[], 1)"
        );
        assert_eq!(arr("select f.v [1:2]"), "select ($1::int4[]) [1:2]");
        assert_eq!(arr("select v || 3"), "select $1::int4[] || 3");
    }

    #[test]
    fn a_composite_parameters_field_is_read() {
        // gr1 / gr2 answered 2 for row(2, 'z')::fe on PostgreSQL 15.19.
        assert_eq!(fields("select r.a"), "select ($1::fe).a");
        assert_eq!(fields("select f.r.a"), "select ($1::fe).a");
        assert_eq!(fields("select R . \"b\""), "select ($1::fe). \"b\"");
        assert_eq!(
            fields("select count(*) from fe where fe.a = r.a"),
            "select count(*) from fe where fe.a = ($1::fe).a"
        );
        // A relation `r` without the field: the parameter's (gr6 answered
        // 'z'); a schema's `s.r` is not `f.r`.
        assert_eq!(fields("select r.b from r"), "select ($1::fe).b from r");
        assert_eq!(
            fields("select f.r.a from s.r"),
            "select ($1::fe).a from s.r"
        );
    }

    #[test]
    fn a_column_or_a_missing_field_is_left() {
        for sql in [
            // A relation known as `r` with the column comes first (gr5
            // answered the table's 99), as does a table `f.r`.
            "select r.a from r",
            "select r.a from fe r",
            "select r.a from (select 1 as a) r",
            "select f.r.a from f.r",
            // No such field, or not a composite: the parser's 42P01.
            "select r.zz",
            "select p.a",
            // Not a reference to a field.
            "select 'r.a'",
            "select r.a.b",
            "select g.r.a",
            "select r.*",
        ] {
            assert_eq!(fields(sql), sql);
        }
    }

    #[test]
    fn a_column_comes_before_a_parameter() {
        assert_eq!(bind("select a from fe", &["a"]), "select a from fe");
        assert_eq!(bind("select a from fs, fe", &["a"]), "select a from fs, fe");
        assert_eq!(bind("select b from fs", &["b"]), "select $1::int4 from fs");
        assert_eq!(bind("select a", &["a"]), "select $1::int4");
    }

    #[test]
    fn only_a_column_reference_is_a_parameter() {
        assert_eq!(bind("select 1 as p", &["p"]), "select 1 as p");
        assert_eq!(bind("select 'p', p", &["p"]), "select 'p', $1::int4");
        assert_eq!(bind("select p(1)", &["p"]), "select p(1)");
        assert_eq!(
            bind("select a as p from fe order by p", &["p"]),
            "select a as p from fe order by p"
        );
    }

    #[test]
    fn scope_follows_query_levels() {
        // A subquery sees the enclosing level's columns.
        assert_eq!(
            bind("select (select a from fs where c = b) from fe", &["b"]),
            "select (select a from fs where c = b) from fe"
        );
        // A non-LATERAL FROM subquery does not see its siblings.
        assert_eq!(
            bind("select * from fe, (select b) s", &["b"]),
            "select * from fe, (select $1::int4) s"
        );
        assert_eq!(
            bind("select * from fe, lateral (select b) s", &["b"]),
            "select * from fe, lateral (select b) s"
        );
        // A CTE's columns, and a function's.
        assert_eq!(
            bind("with w(x) as (select 1) select x, y from w", &["x", "y"]),
            "with w(x) as (select 1) select x, $2::int4 from w"
        );
        assert_eq!(
            bind("select i * n from generate_series(1, n) i", &["i", "n"]),
            "select i * $2::int4 from generate_series(1, $2::int4) i"
        );
        assert_eq!(
            bind("select key, k from jsonb_each(j)", &["key", "k", "j"]),
            "select key, $2::int4 from jsonb_each($3::int4)"
        );
    }

    #[test]
    fn a_star_is_its_items_columns() {
        assert_eq!(
            bind("select b from (select * from fs) s", &["b"]),
            "select $1::int4 from (select * from fs) s"
        );
        assert_eq!(
            bind("select a from (select * from fe) s", &["a"]),
            "select a from (select * from fe) s"
        );
        assert_eq!(
            bind(
                "with w as (select * from fe) select b, c from w",
                &["b", "c"]
            ),
            "with w as (select * from fe) select b, $2::int4 from w"
        );
        assert_eq!(
            bind("select c from (select fe.* from fe, fs) s", &["c"]),
            "select $1::int4 from (select fe.* from fe, fs) s"
        );
        assert_eq!(
            bind(
                "select c from (select a from fe union select a from fs) s",
                &["c"]
            ),
            "select $1::int4 from (select a from fe union select a from fs) s"
        );
    }

    #[test]
    fn a_write_target_is_in_scope_where_postgresql_analyses_against_it() {
        assert_eq!(
            bind("insert into fs values (a, 'i') returning a * 2", &["a"]),
            "insert into fs values ($1::int4, 'i') returning a * 2"
        );
        assert_eq!(
            bind("update fs set a = p where c = 'x' returning p", &["p"]),
            "update fs set a = $1::int4 where c = 'x' returning $1::int4"
        );
        assert_eq!(
            bind("delete from fe where a = b", &["b"]),
            "delete from fe where a = b"
        );
    }

    #[test]
    fn a_qualified_parameter() {
        assert_eq!(
            bind("select f.p from fe", &["p"]),
            "select $1::int4 from fe"
        );
        // A relation known as `f` with a column `p` wins.
        assert_eq!(bind("select f.p from f", &["p"]), "select f.p from f");
        assert_eq!(
            bind("select f.p from fe f", &["p"]),
            "select $1::int4 from fe f"
        );
    }

    /// RP-451's cases, each measured on PostgreSQL 15.19: `fname.p` is the
    /// parameter at any depth, unless a relation known as `fname` may have
    /// a column `p`.
    #[test]
    fn a_qualified_parameter_is_bound_at_any_depth() {
        assert_eq!(bind("select f.p", &["p"]), "select $1::int4");
        assert_eq!(
            bind("select F . P + \"f\".p", &["p"]),
            "select $1::int4 + $1::int4"
        );
        assert_eq!(
            bind("select exists (select f.p from fs)", &["p"]),
            "select exists (select $1::int4 from fs)"
        );
        assert_eq!(
            bind("select f.p from (values (4)) f", &["p"]),
            "select $1::int4 from (values (4)) f"
        );
        assert_eq!(
            bind("select f.p from (select 1 as z union select 2) f", &["p"]),
            "select $1::int4 from (select 1 as z union select 2) f"
        );
        for sql in [
            "select f.p from (select * from f) f",
            // A relation whose columns are not known here may have it.
            "select f.p from some_view f",
            "select f.p from g() f",
        ] {
            assert_eq!(bind(sql, &["p"]), sql);
        }
        for sql in [
            "select 'f.p'",
            "select f.q",
            "select g.p from g",
            // A field of a composite parameter, and a qualified call.
            "select f.p.a",
            "select f.p(1)",
        ] {
            assert_eq!(bind(sql, &["p"]), sql);
        }
    }

    #[test]
    fn a_utility_statement_is_not_analysed() {
        assert_eq!(
            bind("create table t (a int check (a > 0))", &["a"]),
            "create table t (a int check (a > 0))"
        );
        assert_eq!(bind("call pr(a)", &["a"]), "call pr($1::int4)");
    }
}
