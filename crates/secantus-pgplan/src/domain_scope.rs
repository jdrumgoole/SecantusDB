//! Which column a reference names, and whether it is of a domain, across a
//! whole FROM list: its tables, the WITH items it names, its FROM
//! subqueries and the tables inside its JOINs (RP-657).
//!
//! A column declared with a domain is OF the domain (`exprType` of its
//! Var) however the query reaches it: through a join, a WITH, a FROM
//! subquery, or a set operation whose branches are all of the domain
//! (`select_common_type` keeps a type every input shares). The rewrites
//! that cast such a column to its domain (`rewrite_domain_typeof`,
//! `cast_array_subquery_column`) ask this scope which domain, if any, a
//! reference is of. A reference it cannot be sure of -- a bare name more
//! than one FROM item may have, a FROM item whose columns it does not know
//! -- is of no domain here, as before RP-657.

use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{Node, SelectStmt, SetOperation};
use secantus_pgcatalog::TableDef;

/// A FROM item's or a query's columns, in order: each one's name and its
/// domain, if it is of one.
pub(crate) type Columns = Vec<(String, Option<String>)>;

/// The WITH items in scope, innermost last: each one's name and its
/// columns (`None` when they are not known).
type Ctes = Vec<(String, Option<Columns>)>;

/// The FROM items a select's expressions see.
pub(crate) struct Scope {
    /// Each item's reference name and its columns (`None` when they are
    /// not known).
    items: Vec<(String, Option<Columns>)>,
    /// A bare name cannot be resolved: an item's columns are unknown, or a
    /// join merges columns (`USING`, `NATURAL`) or hides its tables behind
    /// an alias.
    opaque: bool,
}

impl Scope {
    /// The scope of one table referred to as `alias`.
    pub(crate) fn single(alias: String, def: &TableDef) -> Scope {
        Scope {
            items: vec![(alias, Some(table_columns(def)))],
            opaque: false,
        }
    }

    /// The scope of `s`'s FROM list, with `ctes` (the enclosing WITH items)
    /// and `s`'s own WITH in scope.
    fn of_select(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>, ctes: &Ctes) -> Scope {
        let ctes = with_ctes(s, lookup, ctes);
        let mut scope = Scope {
            items: Vec::new(),
            opaque: false,
        };
        for item in &s.from_clause {
            scope.add(item, lookup, &ctes);
        }
        scope
    }

    fn add(&mut self, item: &Node, lookup: &dyn Fn(&str) -> Option<TableDef>, ctes: &Ctes) {
        match item.node.as_ref() {
            Some(N::RangeVar(rv)) => {
                let cte = rv
                    .schemaname
                    .is_empty()
                    .then(|| ctes.iter().rev().find(|(n, _)| *n == rv.relname))
                    .flatten();
                let columns = match cte {
                    Some((_, columns)) => columns.clone(),
                    None => lookup(&super::relation_name(rv)).map(|def| table_columns(&def)),
                };
                let (name, columns) = aliased(rv.relname.clone(), rv.alias.as_ref(), columns);
                self.push(name, columns);
            }
            Some(N::RangeSubselect(sub)) => {
                let columns = match sub.subquery.as_deref().and_then(|q| q.node.as_ref()) {
                    Some(N::SelectStmt(q)) => output_columns(q, lookup, ctes),
                    _ => None,
                };
                match sub.alias.as_ref() {
                    Some(a) => {
                        let (name, columns) = aliased(a.aliasname.clone(), Some(a), columns);
                        self.push(name, columns);
                    }
                    None => self.opaque = true,
                }
            }
            Some(N::JoinExpr(j)) => {
                if j.alias.is_some() {
                    // Its tables are hidden behind the alias, and its
                    // columns are the join's, which are not tracked here.
                    self.opaque = true;
                    return;
                }
                if j.is_natural || !j.using_clause.is_empty() {
                    self.opaque = true;
                }
                for side in [j.larg.as_deref(), j.rarg.as_deref()].into_iter().flatten() {
                    self.add(side, lookup, ctes);
                }
            }
            _ => self.opaque = true,
        }
    }

    fn push(&mut self, name: String, columns: Option<Columns>) {
        self.opaque |= columns.is_none();
        self.items.push((name, columns));
    }

    /// Whether any column in scope is of a domain.
    pub(crate) fn has_domains(&self) -> bool {
        self.items
            .iter()
            .flat_map(|(_, c)| c.iter().flatten())
            .any(|(_, d)| d.is_some())
    }

    /// The domain of the column `node` is a bare reference to, when it
    /// names exactly one column in scope and that one is of a domain.
    pub(crate) fn domain_of(&self, node: &Node) -> Option<&str> {
        let Some(N::ColumnRef(c)) = node.node.as_ref() else {
            return None;
        };
        let name = super::column_ref_name(c)?;
        let candidates: Vec<&Columns> = match c.fields.as_slice() {
            [_] if !self.opaque => self.items.iter().flat_map(|(_, c)| c.as_ref()).collect(),
            [q, _] => {
                let Some(N::String(q)) = q.node.as_ref() else {
                    return None;
                };
                let mut named = self.items.iter().filter(|(n, _)| *n == q.sval);
                match (named.next(), named.next()) {
                    (Some((_, Some(columns))), None) => vec![columns],
                    _ => return None,
                }
            }
            _ => return None,
        };
        let mut matching = candidates
            .into_iter()
            .flat_map(|c| c.iter())
            .filter(|(n, _)| *n == name);
        match (matching.next(), matching.next()) {
            (Some((_, Some(domain))), None) => Some(domain.as_str()),
            _ => None,
        }
    }
}

/// `def`'s columns, in order, each with its domain.
fn table_columns(def: &TableDef) -> Columns {
    let domains = super::domain_columns_of(def);
    def.columns
        .iter()
        .map(|c| (c.name.clone(), domains.get(&c.name).cloned()))
        .collect()
}

/// A FROM item's reference name and columns under its alias: the alias's
/// name, and its column names over the first columns, in order.
fn aliased(
    name: String,
    alias: Option<&pg_query::protobuf::Alias>,
    columns: Option<Columns>,
) -> (String, Option<Columns>) {
    let Some(alias) = alias else {
        return (name, columns);
    };
    let columns = columns.and_then(|mut columns| {
        if alias.colnames.len() > columns.len() {
            return None;
        }
        for (column, rename) in columns.iter_mut().zip(&alias.colnames) {
            column.0 = string_value(rename)?;
        }
        Some(columns)
    });
    (alias.aliasname.clone(), columns)
}

fn string_value(n: &Node) -> Option<String> {
    match n.node.as_ref() {
        Some(N::String(s)) => Some(s.sval.clone()),
        _ => None,
    }
}

/// `ctes` with `s`'s own WITH items added, each one's columns measured in
/// the scope of the items before it. A recursive WITH's items are of no
/// known columns.
fn with_ctes(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>, ctes: &Ctes) -> Ctes {
    let mut out = ctes.clone();
    let Some(with) = s.with_clause.as_ref() else {
        return out;
    };
    for cte in &with.ctes {
        let Some(N::CommonTableExpr(c)) = cte.node.as_ref() else {
            continue;
        };
        let columns = if with.recursive {
            None
        } else {
            match c.ctequery.as_deref().and_then(|q| q.node.as_ref()) {
                Some(N::SelectStmt(q)) => output_columns(q, lookup, &out).and_then(|columns| {
                    let renamed: Option<Vec<String>> =
                        c.aliascolnames.iter().map(string_value).collect();
                    let renamed = renamed?;
                    if renamed.len() > columns.len() {
                        return None;
                    }
                    let mut columns = columns;
                    for (column, name) in columns.iter_mut().zip(renamed) {
                        column.0 = name;
                    }
                    Some(columns)
                }),
                _ => None,
            }
        };
        out.push((c.ctename.clone(), columns));
    }
    out
}

/// The columns `s` returns, each with its domain when it is a bare
/// reference to a domain column (a set operation's column only when every
/// branch's is of the same domain). `None` when they are not known.
pub(crate) fn output_columns(
    s: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
) -> Option<Columns> {
    if s.op != SetOperation::SetopNone as i32 {
        let ctes = with_ctes(s, lookup, ctes);
        let left = output_columns(s.larg.as_deref()?, lookup, &ctes)?;
        let right = output_columns(s.rarg.as_deref()?, lookup, &ctes)?;
        if left.len() != right.len() {
            return None;
        }
        return Some(
            left.into_iter()
                .zip(right)
                .map(|((name, l), (_, r))| (name, if l == r { l } else { None }))
                .collect(),
        );
    }
    if !s.values_lists.is_empty() {
        return None;
    }
    let scope = Scope::of_select(s, lookup, ctes);
    let mut out = Columns::new();
    for target in &s.target_list {
        let Some(N::ResTarget(rt)) = target.node.as_ref() else {
            return None;
        };
        let val = rt.val.as_deref()?;
        if let Some(N::ColumnRef(c)) = val.node.as_ref() {
            if matches!(
                c.fields.last().and_then(|f| f.node.as_ref()),
                Some(N::AStar(_))
            ) {
                out.extend(star_columns(&scope, &c.fields)?);
                continue;
            }
        }
        let name = if rt.name.is_empty() {
            match val.node.as_ref() {
                Some(N::ColumnRef(c)) => super::column_ref_name(c)?,
                _ => "?column?".to_string(),
            }
        } else {
            rt.name.clone()
        };
        out.push((name, scope.domain_of(val).map(str::to_string)));
    }
    Some(out)
}

/// The columns `*` or `q.*` stands for.
fn star_columns(scope: &Scope, fields: &[Node]) -> Option<Columns> {
    match fields {
        [_] if !scope.opaque => Some(
            scope
                .items
                .iter()
                .flat_map(|(_, c)| c.iter().flatten().cloned())
                .collect(),
        ),
        [q, _] => {
            let q = string_value(q)?;
            let mut named = scope.items.iter().filter(|(n, _)| *n == q);
            match (named.next(), named.next()) {
                (Some((_, Some(columns))), None) => Some(columns.clone()),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The scope of `s`'s FROM list with `s`'s own WITH in scope.
pub(crate) fn scope_of(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Scope {
    Scope::of_select(s, lookup, &Ctes::new())
}

/// Cast the first column of each leaf select of `s` to `domain`. Every leaf
/// is checked first: none is cast unless each one's first target is a
/// bare reference to a column of `domain`.
pub(crate) fn cast_first_columns(
    s: &mut SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> bool {
    let Some(columns) = output_columns(s, lookup, &Ctes::new()) else {
        return false;
    };
    let [(_, Some(domain))] = columns.as_slice() else {
        return false;
    };
    let domain = domain.clone();
    let mut leaves = Vec::new();
    collect_leaves(s, lookup, &Ctes::new(), &mut leaves);
    if leaves.iter().any(|d| d.as_deref() != Some(domain.as_str())) {
        return false;
    }
    cast_leaves(s, &domain);
    true
}

/// Each leaf select's first target's domain, in order.
fn collect_leaves(
    s: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
    out: &mut Vec<Option<String>>,
) {
    if s.op != SetOperation::SetopNone as i32 {
        let ctes = with_ctes(s, lookup, ctes);
        for side in [s.larg.as_deref(), s.rarg.as_deref()].into_iter().flatten() {
            collect_leaves(side, lookup, &ctes, out);
        }
        return;
    }
    let scope = Scope::of_select(s, lookup, ctes);
    let domain = match s.target_list.as_slice() {
        [target] => match target.node.as_ref() {
            Some(N::ResTarget(rt)) => rt
                .val
                .as_deref()
                .and_then(|v| scope.domain_of(v))
                .map(str::to_string),
            _ => None,
        },
        _ => None,
    };
    out.push(domain);
}

/// Cast every leaf select's one target to `domain` (a relabel).
fn cast_leaves(s: &mut SelectStmt, domain: &str) {
    if s.op != SetOperation::SetopNone as i32 {
        for side in [s.larg.as_deref_mut(), s.rarg.as_deref_mut()]
            .into_iter()
            .flatten()
        {
            cast_leaves(side, domain);
        }
        return;
    }
    if let [target] = s.target_list.as_mut_slice() {
        if let Some(N::ResTarget(rt)) = target.node.as_mut() {
            if let Some(v) = rt.val.as_deref_mut() {
                let arg = v.clone();
                *v = super::relabel_cast(arg, domain);
            }
        }
    }
}
