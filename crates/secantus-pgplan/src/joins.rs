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
    let mut scope = Scope::default();
    let mut tree: Option<JoinNode> = None;
    for item in &s.from_clause {
        let (node, item_scope) = build(item, lookup, params, &mut scope.aliases)?;
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
) -> Result<(JoinNode, Scope)> {
    match item.node.as_ref() {
        Some(N::JoinExpr(j)) => build_join(j, lookup, params, aliases),
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
            leaf(
                pg_query::protobuf::Node {
                    node: Some(N::RangeVar(rv)),
                },
                &alias,
                &colnames,
                lookup,
                params,
                aliases,
            )
        }
        Some(N::RangeSubselect(rs)) => {
            if rs.lateral {
                return Err(Error::Unsupported("a LATERAL subquery in FROM".into()));
            }
            let src = plan_from_subquery(rs, lookup, params)?;
            leaf_from_source(src.plan, src.def, &src.alias, lookup, aliases)
        }
        Some(N::RangeFunction(rf)) => {
            if rf.lateral {
                return Err(Error::Unsupported("a LATERAL function in FROM".into()));
            }
            // A function call's own alias names the source; without one,
            // PostgreSQL names it after the function.
            let alias = rf
                .alias
                .as_ref()
                .map(|a| a.aliasname.clone())
                .filter(|a| !a.is_empty())
                .or_else(|| range_function_name(rf))
                .ok_or_else(|| Error::Unsupported("this FROM function".into()))?;
            leaf(item.clone(), &alias, &[], lookup, params, aliases)
        }
        _ => Err(Error::Unsupported("this JOIN side".into())),
    }
}

fn range_function_name(rf: &pg_query::protobuf::RangeFunction) -> Option<String> {
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
    let select = pg_query::protobuf::SelectStmt {
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
    let (left, lscope) = build(larg, lookup, params, aliases)?;
    let (right, rscope) = build(rarg, lookup, params, aliases)?;
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
