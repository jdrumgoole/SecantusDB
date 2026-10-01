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
        /// `(left key, right key)` column equalities that are conjuncts of
        /// the ON: candidates are found by hashing on them, and `on` still
        /// decides each pair.
        equi: Vec<(String, String)>,
        /// USING / NATURAL columns: `(merged key, left key, right key)`.
        merged: Vec<(String, String, String)>,
        /// Every key each side produces, so an outer join can NULL-extend.
        left_keys: Vec<String>,
        right_keys: Vec<String>,
    },
}

/// A join as a statement the executor materialises: its tree and the def of
/// the rows it produces.
#[derive(Debug, Clone, PartialEq)]
pub struct JoinRows {
    pub tree: JoinNode,
    pub def: TableDef,
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
    /// WHERE conjuncts pushed into a table leaf, by the leaf's alias, while
    /// its join is built (see `pushdown_filters`).
    static PUSHDOWN: std::cell::RefCell<Vec<(String, pg_query::protobuf::Node)>> =
        const { std::cell::RefCell::new(Vec::new()) };
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
    let outer_pushdown = PUSHDOWN.with(|p| p.replace(pushdown_filters(s)));
    let built = build_tree(s, lookup, params);
    PUSHDOWN.with(|p| *p.borrow_mut() = outer_pushdown);
    let (tree, scope) = built?;
    finish_join_source(s, tree, scope)
}

fn build_tree(
    s: &pg_query::protobuf::SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    params: &[Bson],
) -> Result<(JoinNode, Scope)> {
    let mut scope = Scope::default();
    let mut tree: Option<JoinNode> = None;
    for item in &s.from_clause {
        let left = (tree.is_some()).then(|| scope.clone());
        let (node, item_scope) = build(item, lookup, params, &mut scope.aliases, left.as_ref())?;
        tree = Some(match tree {
            None => node,
            // `FROM a, b`: a cross join, left to right.
            Some(left) => JoinNode::Join {
                kind: JoinKind::Inner,
                left_keys: keys_of(&left),
                right_keys: keys_of(&node),
                left: Box::new(left),
                right: Box::new(node),
                on: None,
                equi: Vec::new(),
                merged: Vec::new(),
            },
        });
        scope.cols.extend(item_scope.cols);
    }
    let tree = tree.ok_or_else(|| Error::Parse("empty FROM".into()))?;
    Ok((tree, scope))
}

fn finish_join_source(
    s: &pg_query::protobuf::SelectStmt,
    tree: JoinNode,
    scope: Scope,
) -> Result<pg_query::protobuf::SelectStmt> {
    let def = TableDef::new(
        "",
        scope
            .cols
            .iter()
            .map(|c| Column::new(&c.key, &c.pg_type, false))
            .collect(),
    );
    let name = PLANNED_JOINS.with(|j| {
        let mut j = j.borrow_mut();
        let name = format!("{SEP}join{SEP}{}", j.len());
        j.push((
            name.clone(),
            SubSource {
                alias: String::new(),
                plan: Box::new(Statement::JoinRows(JoinRows {
                    tree,
                    def: def.clone(),
                })),
                def,
            },
        ));
        name
    });
    let mut out = s.clone();
    rewrite_query(&mut out, &scope)?;
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
            rf.alias = keep_column_names(&rf, &alias);
            leaf(
                pg_query::protobuf::Node {
                    node: Some(N::RangeFunction(rf)),
                },
                &alias,
                &colnames,
                lookup,
                params,
                aliases,
            )
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
            [_] => function,
            _ => false,
        };
        if !points_left {
            return Ok(());
        }
        let key = match left.resolve(&parts) {
            Ok(Some(k)) => k,
            Ok(None) => return Ok(()),
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
        // It reads nothing on its left after all.
        return Ok(None);
    }
    // Planned once over typed sample values, for the output's shape; each left
    // row re-plans it with its own.
    let mut sample_params = params.to_vec();
    sample_params.extend(types.iter().map(|t| sample_value_for_type(t)));
    let plan = plan_select(&inner, lookup, &sample_params)?;
    let def = sub_plan_def(&plan, lookup)?;
    let names: Vec<String> = colnames.iter().filter_map(alias_colname).collect();
    let names = scalar_alias(names, function, &alias, def.columns.len());
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
    let function = matches!(select.from_clause[0].node, Some(N::RangeFunction(_)));
    let plan = plan_select(&select, lookup, params)?;
    let mut def = sub_plan_def(&plan, lookup)?;
    let names: Vec<String> = colnames.iter().filter_map(alias_colname).collect();
    let names = scalar_alias(names, function, alias, def.columns.len());
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
    let (left, lscope) = build(larg, lookup, params, aliases, outer_left)?;
    // The right side may be LATERAL over everything to its left.
    let mut visible = outer_left.cloned().unwrap_or_default();
    visible.cols.extend(lscope.cols.iter().cloned());
    visible.aliases.extend(lscope.aliases.iter().cloned());
    let (right, rscope) = build(rarg, lookup, params, aliases, Some(&visible))?;
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
        merged_cols.push(ScopeCol {
            alias: String::new(),
            name: name.clone(),
            key,
            pg_type: l.pg_type.clone(),
            bare: true,
            star: true,
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
            equi.extend(equalities(&q, &left_keys, &right_keys));
            Some(on_expr(&q, &side_scope, params)?)
        }
    };
    Ok((
        JoinNode::Join {
            kind,
            left: Box::new(left),
            right: Box::new(right),
            on,
            equi,
            merged,
            left_keys,
            right_keys,
        },
        scope,
    ))
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

/// `left.x = right.y` conjuncts of an ON, as `(left key, right key)`.
fn equalities(
    node: &pg_query::protobuf::Node,
    left: &[String],
    right: &[String],
) -> Vec<(String, String)> {
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
                (Some(a), Some(b)) if left.contains(&a) && right.contains(&b) => vec![(a, b)],
                (Some(a), Some(b)) if left.contains(&b) && right.contains(&a) => vec![(b, a)],
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
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
        if let Some(key) = scope.resolve(&parts)? {
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
    for o in &mut s.sort_clause {
        rewrite_expr(o, scope, &outputs)?;
    }
    for w in &mut s.window_clause {
        rewrite_expr(w, scope, &[])?;
    }
    Ok(())
}

/// The set-returning functions a select list may call over a column.
const SELECT_LIST_SRFS: &[&str] = &[
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
];

/// `SELECT unnest(ia) FROM t` as the LATERAL join it means: `FROM t, LATERAL
/// unnest(ia) AS __srf0`, the target reading the function's column. A
/// set-returning function in the select list changes the ROW COUNT, which no
/// per-row expression can do; as a lateral source it is just more rows.
///
/// `None` when the select list calls none over a column. Several run in
/// LOCKSTEP (the longest decides the row count, the others pad with NULL), as
/// PostgreSQL runs them: a `ROWS FROM` of them all.
pub(crate) fn select_list_srf(
    s: &pg_query::protobuf::SelectStmt,
) -> Result<Option<pg_query::protobuf::SelectStmt>> {
    let is_srf = |n: Option<&pg_query::protobuf::Node>| match n.and_then(|v| v.node.as_ref()) {
        Some(N::FuncCall(f)) if f.over.is_none() => func_name(f).is_some_and(|name| {
            SELECT_LIST_SRFS.contains(&name.as_str())
                || crate::correlated::user_function_for(&name, &f.args)
                    .is_some_and(|u| u.returns_set)
        }),
        _ => false,
    };
    // A target that IS a set-returning call, or holds one inside an
    // expression (`generate_series(1, 3) * 2`, `jsonb_path_query(...)::text`).
    let holds_srf = |n: Option<&pg_query::protobuf::Node>| -> bool {
        let Some(n) = n else { return false };
        let mut probe = n.clone();
        let mut hit = false;
        let _ = walk_expr(&mut probe, &mut |m| {
            if is_srf(Some(m)) {
                hit = true;
            }
            Ok(())
        });
        hit
    };
    let positions: Vec<usize> = s
        .target_list
        .iter()
        .enumerate()
        .filter(
            |(_, t)| matches!(t.node.as_ref(), Some(N::ResTarget(r)) if holds_srf(r.val.as_deref())),
        )
        .map(|(i, _)| i)
        .collect();
    // With no FROM, a set-returning function ALONE is already its own
    // source; beside another target, or inside an expression, it needs one
    // row to join to.
    let bare_single = s.target_list.len() == 1
        && matches!(s.target_list[0].node.as_ref(), Some(N::ResTarget(r)) if is_srf(r.val.as_deref()));
    if s.from_clause.is_empty() && (positions.is_empty() || bare_single) {
        return Ok(None);
    }
    if positions.is_empty() {
        return Ok(None);
    }
    let mut out = s.clone();
    if out.from_clause.is_empty() {
        let one = pg_query::protobuf::SelectStmt {
            target_list: Vec::new(),
            limit_option: pg_query::protobuf::LimitOption::Default as i32,
            op: pg_query::protobuf::SetOperation::SetopNone as i32,
            ..Default::default()
        };
        out.from_clause.push(pg_query::protobuf::Node {
            node: Some(N::RangeSubselect(Box::new(
                pg_query::protobuf::RangeSubselect {
                    lateral: false,
                    subquery: Some(Box::new(pg_query::protobuf::Node {
                        node: Some(N::SelectStmt(Box::new(one))),
                    })),
                    alias: Some(pg_query::protobuf::Alias {
                        aliasname: "__one".into(),
                        colnames: Vec::new(),
                    }),
                },
            ))),
        });
    }
    // One function's column is `__srf0.__srf0`, as before; several run in
    // LOCKSTEP -- the longest decides the row count, the others pad with
    // NULL -- which is `ROWS FROM (f, g)`, one column each.
    let several = positions.len() > 1;
    let column_name = |k: usize| {
        if several {
            format!("c{k}")
        } else {
            "__srf0".to_string()
        }
    };
    let mut calls = Vec::new();
    for (k, i) in positions.iter().enumerate() {
        let Some(N::ResTarget(rt)) = out.target_list[*i].node.as_mut() else {
            return Ok(None);
        };
        let mut expr = *rt.val.take().expect("checked");
        // The output keeps the name PostgreSQL gives the ORIGINAL
        // expression (`jsonb_path_query` for its `::text` too).
        if rt.name.is_empty() {
            rt.name = expression_column_name(&expr);
        }
        let column = pg_query::protobuf::Node {
            node: Some(N::ColumnRef(pg_query::protobuf::ColumnRef {
                fields: vec![string_node("__srf0"), string_node(&column_name(k))],
                location: -1,
            })),
        };
        // Pull the call out of the expression; the column takes its place.
        let mut call: Option<pg_query::protobuf::Node> = None;
        if is_srf(Some(&expr)) {
            call = Some(std::mem::replace(&mut expr, column.clone()));
        } else {
            walk_expr(&mut expr, &mut |m| {
                if call.is_none() && is_srf(Some(m)) {
                    call = Some(std::mem::replace(m, column.clone()));
                }
                Ok(())
            })?;
        }
        let Some(call) = call else {
            return Ok(None);
        };
        // Two set-returning calls inside ONE expression would also run in
        // lockstep; that shape keeps a refusal rather than a wrong guess.
        let mut another = false;
        walk_expr(&mut expr, &mut |m| {
            another |= is_srf(Some(m));
            Ok(())
        })?;
        if another {
            return Err(Error::Unsupported(
                "several set-returning functions in one expression".into(),
            ));
        }
        rt.val = Some(Box::new(expr));
        calls.push(call);
    }
    let rf = pg_query::protobuf::RangeFunction {
        lateral: true,
        is_rowsfrom: several,
        functions: calls
            .into_iter()
            .map(|call| pg_query::protobuf::Node {
                node: Some(N::List(pg_query::protobuf::List {
                    items: vec![call, pg_query::protobuf::Node { node: None }],
                })),
            })
            .collect(),
        alias: Some(pg_query::protobuf::Alias {
            aliasname: "__srf0".into(),
            colnames: (0..positions.len())
                .map(|k| string_node(&column_name(k)))
                .collect(),
        }),
        ..Default::default()
    };
    out.from_clause.push(pg_query::protobuf::Node {
        node: Some(N::RangeFunction(rf)),
    });
    Ok(Some(out))
}

/// The WHERE conjuncts a table leaf can apply to its own rows, by alias.
///
/// A conjunct qualifies when every column it reads is qualified by ONE
/// alias, and it is built only of operators, casts, constants and parameters
/// (no function call, which may be volatile, and no subquery). Its leaf must
/// be on the PRESERVED side of every outer join above it: filtering the
/// nullable side before the join would keep the NULL-extended rows
/// PostgreSQL's WHERE drops. The conjunct also stays in the outer WHERE, so
/// the answer is the same whether or not the leaf applied it -- the leaf
/// just reads fewer rows, and can reach an index.
fn pushdown_filters(s: &pg_query::protobuf::SelectStmt) -> Vec<(String, pg_query::protobuf::Node)> {
    let Some(w) = s.where_clause.as_deref() else {
        return Vec::new();
    };
    let mut preserved = Vec::new();
    for item in &s.from_clause {
        preserved_aliases(item, false, &mut preserved);
    }
    let mut conjuncts = Vec::new();
    split_and(w, &mut conjuncts);
    conjuncts
        .into_iter()
        .filter_map(|c| {
            let alias = single_alias(c)?;
            preserved.contains(&alias).then(|| {
                let mut c = c.clone();
                strip_qualifier(&mut c);
                (alias, c)
            })
        })
        .collect()
}

/// The table aliases below `item` that no outer join makes nullable.
fn preserved_aliases(item: &pg_query::protobuf::Node, nullable: bool, out: &mut Vec<String>) {
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
                Ok(JoinType::JoinLeft) => (false, true),
                Ok(JoinType::JoinRight) => (true, false),
                _ => (true, true),
            };
            if let Some(larg) = j.larg.as_deref() {
                preserved_aliases(larg, nullable || l, out);
            }
            if let Some(rarg) = j.rarg.as_deref() {
                preserved_aliases(rarg, nullable || r, out);
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

/// The one alias every column of `n` is qualified by, when `n` is a shape
/// that may be pushed.
fn single_alias(n: &pg_query::protobuf::Node) -> Option<String> {
    fn walk(n: &pg_query::protobuf::Node, alias: &mut Option<String>) -> bool {
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
                match parts.as_deref() {
                    Some([q, _]) => match alias {
                        Some(a) => a == q,
                        None => {
                            *alias = Some(q.to_string());
                            true
                        }
                    },
                    _ => false,
                }
            }
            Some(N::AConst(_)) | Some(N::ParamRef(_)) => true,
            Some(N::TypeCast(t)) => t.arg.as_deref().is_some_and(|a| walk(a, alias)),
            Some(N::AExpr(e)) => {
                e.lexpr.as_deref().is_none_or(|x| walk(x, alias))
                    && e.rexpr.as_deref().is_none_or(|x| walk(x, alias))
            }
            Some(N::BoolExpr(b)) => b.args.iter().all(|a| walk(a, alias)),
            Some(N::NullTest(t)) => t.arg.as_deref().is_some_and(|a| walk(a, alias)),
            Some(N::BooleanTest(t)) => t.arg.as_deref().is_some_and(|a| walk(a, alias)),
            Some(N::List(l)) => l.items.iter().all(|a| walk(a, alias)),
            Some(N::AArrayExpr(a)) => a.elements.iter().all(|x| walk(x, alias)),
            _ => false,
        }
    }
    let mut alias = None;
    (walk(n, &mut alias)).then_some(alias).flatten()
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
