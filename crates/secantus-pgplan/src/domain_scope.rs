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
//!
//! A write asks the same question of its values (RP-664): PostgreSQL checks
//! a domain's constraints where a value is COERCED into the domain, and
//! `coerce_to_target_type` returns an expression already of the target
//! type as it is. So a value of the column's own domain -- the column
//! itself, a sibling of that domain, a cast to it, a subquery's column of
//! it, a COALESCE of them -- is written with no check, and a row stored
//! before a CHECK added `NOT VALID` can still be updated and copied.

use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{Node, SelectStmt, SetOperation};
use secantus_pgcatalog::TableDef;

use super::{domains, type_name_of, ConflictAction, Statement};

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
    /// The domain each parameter (`$1`, ...) is declared of, if it is one:
    /// a PL/pgSQL variable bound into an expression is of its declared
    /// type (RP-670). Empty for a statement's own scope.
    params: Vec<Option<String>>,
}

impl Scope {
    /// The scope of one table referred to as `alias`.
    pub(crate) fn single(alias: String, def: &TableDef) -> Scope {
        Scope {
            items: vec![(alias, Some(table_columns(def)))],
            opaque: false,
            params: Vec::new(),
        }
    }

    /// The scope of `s`'s FROM list, with `ctes` (the enclosing WITH items)
    /// and `s`'s own WITH in scope.
    fn of_select(s: &SelectStmt, lookup: &dyn Fn(&str) -> Option<TableDef>, ctes: &Ctes) -> Scope {
        Scope::of_from(&s.from_clause, lookup, &with_ctes(s, lookup, ctes))
    }

    /// The scope of a FROM list, with `ctes` in scope.
    fn of_from(from: &[Node], lookup: &dyn Fn(&str) -> Option<TableDef>, ctes: &Ctes) -> Scope {
        let mut scope = Scope {
            items: Vec::new(),
            opaque: false,
            params: Vec::new(),
        };
        for item in from {
            scope.add(item, lookup, ctes);
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

    pub(crate) fn push(&mut self, name: String, columns: Option<Columns>) {
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
pub(crate) fn table_columns(def: &TableDef) -> Columns {
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
    with_clause_ctes(s.with_clause.as_ref(), lookup, ctes)
}

/// `ctes` with the items of `with` (a select's or a write's) added.
fn with_clause_ctes(
    with: Option<&pg_query::protobuf::WithClause>,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
) -> Ctes {
    let mut out = ctes.clone();
    let Some(with) = with else {
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

/// The columns `s` returns, each with the domain its expression is of
/// (`expr_domain`; a set operation's column only when every branch's is of
/// the same domain). `None` when they are not known.
pub(crate) fn output_columns(
    s: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
) -> Option<Columns> {
    output_columns_with(s, lookup, ctes, &[])
}

/// [`output_columns`], with `params` the domain each parameter is of.
fn output_columns_with(
    s: &SelectStmt,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
    params: &[Option<String>],
) -> Option<Columns> {
    if s.op != SetOperation::SetopNone as i32 {
        let ctes = with_ctes(s, lookup, ctes);
        let left = output_columns_with(s.larg.as_deref()?, lookup, &ctes, params)?;
        let right = output_columns_with(s.rarg.as_deref()?, lookup, &ctes, params)?;
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
    let ctes = with_ctes(s, lookup, ctes);
    let mut scope = Scope::of_from(&s.from_clause, lookup, &ctes);
    scope.params = params.to_vec();
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
        out.push((name, expr_domain(val, &scope, lookup, &ctes)));
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

/// Every type in `domains` is the same domain: `select_common_type`
/// resolves to a domain only when all its inputs are of exactly that type,
/// and otherwise to their base types.
fn common_domain(mut domains: impl Iterator<Item = Option<String>>) -> Option<String> {
    let first = domains.next()??;
    for d in domains {
        if !d.is_some_and(|d| domains::same_domain(&d, &first)) {
            return None;
        }
    }
    Some(first)
}

/// The domain `node` is already of (`exprType`), when it is one: a column
/// of a domain (`Scope::domain_of`), a cast to a domain, a scalar
/// subquery's column of one, or a COALESCE, GREATEST / LEAST or CASE whose
/// every input is of one domain.
fn expr_domain(
    node: &Node,
    scope: &Scope,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
) -> Option<String> {
    let of = |e: &Node| expr_domain(e, scope, lookup, ctes);
    match node.node.as_ref()? {
        N::TypeCast(tc) => {
            let t = tc.type_name.as_ref()?;
            if !t.typmods.is_empty() || !t.array_bounds.is_empty() || t.setof || t.pct_type {
                return None;
            }
            let name = type_name_of(t);
            domains::user_domain(&name).map(|_| name)
        }
        N::ColumnRef(_) => scope.domain_of(node).map(str::to_string),
        N::ParamRef(p) => usize::try_from(p.number - 1)
            .ok()
            .and_then(|i| scope.params.get(i)?.clone()),
        N::CoalesceExpr(c) => common_domain(c.args.iter().map(of)),
        N::MinMaxExpr(m) => common_domain(m.args.iter().map(of)),
        N::CaseExpr(c) => {
            // Without an ELSE the default is an untyped NULL, which leaves
            // the common type its inputs' base type.
            let default = c.defresult.as_deref()?;
            let results = c.args.iter().filter_map(|w| match w.node.as_ref() {
                Some(N::CaseWhen(w)) => w.result.as_deref(),
                _ => None,
            });
            common_domain(std::iter::once(default).chain(results).map(of))
        }
        N::SubLink(sl) => {
            use pg_query::protobuf::SubLinkType;
            if SubLinkType::try_from(sl.sub_link_type) != Ok(SubLinkType::ExprSublink) {
                return None;
            }
            sublink_column(sl, 0, lookup, ctes)
        }
        _ => None,
    }
}

/// The domain the one target of `sql`, a `SELECT <expr>` whose parameters
/// are declared of `types`, is already of (`exprType`), when it is one.
/// PL/pgSQL's `exec_assign_value` casts an assigned value from that type
/// to its target's, so a value already of the target's domain is no
/// coercion and checks nothing (RP-670).
pub fn select_target_domain(
    sql: &str,
    types: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<String> {
    let parsed = pg_query::parse(sql).ok()?;
    let stmt = parsed.protobuf.stmts.first()?.stmt.as_deref()?;
    let Some(N::SelectStmt(s)) = stmt.node.as_ref() else {
        return None;
    };
    let [target] = s.target_list.as_slice() else {
        return None;
    };
    let Some(N::ResTarget(rt)) = target.node.as_ref() else {
        return None;
    };
    let ctes = with_ctes(s, lookup, &Ctes::new());
    let mut scope = Scope::of_from(&s.from_clause, lookup, &ctes);
    scope.params = types
        .iter()
        .map(|t| domains::user_domain(t).map(|_| t.clone()))
        .collect();
    expr_domain(rt.val.as_deref()?, &scope, lookup, &ctes)
}

/// The domain each column a query's row returns is of, in order, for
/// `exec_move_row`: a column of the domain its `INTO` variable is declared
/// of is assigned with no coercion, as PostgreSQL's `exec_cast_value` skips
/// one between equal types (RP-694). A parameter is of the domain `types`
/// names. Empty when the columns are not known (not a SELECT, a VALUES
/// list).
pub fn select_output_domains(
    sql: &str,
    types: &[String],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Vec<Option<String>> {
    let Ok(parsed) = pg_query::parse(sql) else {
        return Vec::new();
    };
    let Some(N::SelectStmt(s)) = parsed
        .protobuf
        .stmts
        .first()
        .and_then(|r| r.stmt.as_deref())
        .and_then(|n| n.node.as_ref())
    else {
        return Vec::new();
    };
    let params: Vec<Option<String>> = types
        .iter()
        .map(|t| domains::user_domain(t).map(|_| t.clone()))
        .collect();
    output_columns_with(s, lookup, &Ctes::new(), &params)
        .map(|columns| columns.into_iter().map(|(_, d)| d).collect())
        .unwrap_or_default()
}

/// The domain column `i` of a subquery's row is of.
fn sublink_column(
    sl: &pg_query::protobuf::SubLink,
    i: usize,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
) -> Option<String> {
    let Some(N::SelectStmt(s)) = sl.subselect.as_deref().and_then(|q| q.node.as_ref()) else {
        return None;
    };
    output_columns(s, lookup, ctes)?.into_iter().nth(i)?.1
}

/// The scope of a write's expressions: its target, by the name it is
/// referred to by, then its FROM (or USING) list, under its WITH clause.
fn write_scope(
    target: &pg_query::protobuf::RangeVar,
    def: &TableDef,
    from: &[Node],
    with: Option<&pg_query::protobuf::WithClause>,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> (Scope, Ctes) {
    let ctes = with_clause_ctes(with, lookup, &Ctes::new());
    let mut scope = Scope {
        items: Vec::new(),
        opaque: false,
        params: Vec::new(),
    };
    let (name, columns) = aliased(
        target.relname.clone(),
        target.alias.as_ref(),
        Some(table_columns(def)),
    );
    scope.push(name, columns);
    for item in from {
        scope.add(item, lookup, &ctes);
    }
    (scope, ctes)
}

/// Whether `domain` names a domain and `other` the same one.
fn is_domain(domain: Option<&str>, other: &str) -> bool {
    domain.is_some_and(|d| domains::same_domain(d, other))
}

/// What a write's statement, as written, says of its values' domains: for
/// the planned `Update` / `Insert` of `table`.
pub(crate) struct WriteDomains {
    table: String,
    /// An UPDATE's `of_domain`.
    update: Vec<String>,
    /// An INSERT's `source_domains`.
    source: Vec<Option<String>>,
    /// An INSERT's ON CONFLICT DO UPDATE `of_domain`.
    conflict: Vec<String>,
}

impl WriteDomains {
    /// Record these on the statement planned from the write; nothing when
    /// it writes another table (a view's rewrite onto its base).
    pub(crate) fn apply(self, st: &mut Statement) {
        match st {
            Statement::Update(u) if u.table == self.table => u.of_domain = self.update,
            Statement::Insert(i) if i.table == self.table => {
                i.source_domains = self.source;
                if let Some(ConflictAction::Update { of_domain, .. }) =
                    i.on_conflict.as_mut().map(|c| &mut c.action)
                {
                    *of_domain = self.conflict;
                }
            }
            _ => {}
        }
    }
}

/// The domains of an UPDATE's or an INSERT's values, read from the
/// statement as written, before its subqueries run and its FROM list is
/// rewritten into them; `None` for any other statement, or a target with
/// no domain column.
pub(crate) fn of_write(
    node: &Node,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
) -> Option<WriteDomains> {
    match node.node.as_ref()? {
        N::UpdateStmt(u) => {
            let rv = u.relation.as_ref()?;
            let def = lookup(&rv.relname)?;
            if super::domain_columns_of(&def).is_empty() {
                return None;
            }
            let (scope, ctes) =
                write_scope(rv, &def, &u.from_clause, u.with_clause.as_ref(), lookup);
            Some(WriteDomains {
                table: rv.relname.clone(),
                update: assigned_of_domain(&u.target_list, &def, &scope, lookup, &ctes),
                source: Vec::new(),
                conflict: Vec::new(),
            })
        }
        N::InsertStmt(i) => {
            let rv = i.relation.as_ref()?;
            let def = lookup(&rv.relname)?;
            if super::domain_columns_of(&def).is_empty() {
                return None;
            }
            let ctes = with_clause_ctes(i.with_clause.as_ref(), lookup, &Ctes::new());
            let source = match i.select_stmt.as_deref().and_then(|s| s.node.as_ref()) {
                Some(N::SelectStmt(s)) if s.values_lists.is_empty() => {
                    output_columns(s, lookup, &ctes)
                        .map(|cols| cols.into_iter().map(|(_, d)| d).collect())
                        .unwrap_or_default()
                }
                _ => Vec::new(),
            };
            let conflict = match i.on_conflict_clause.as_deref() {
                Some(c) => {
                    let (mut scope, ctes) = write_scope(rv, &def, &[], None, lookup);
                    scope.push("excluded".into(), Some(table_columns(&def)));
                    assigned_of_domain(&c.target_list, &def, &scope, lookup, &ctes)
                }
                None => Vec::new(),
            };
            Some(WriteDomains {
                table: rv.relname.clone(),
                update: Vec::new(),
                source,
                conflict,
            })
        }
        _ => None,
    }
}

/// The stored fields of the domain columns a SET list assigns a value
/// already of the column's domain.
fn assigned_of_domain(
    targets: &[Node],
    def: &TableDef,
    scope: &Scope,
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    ctes: &Ctes,
) -> Vec<String> {
    let mut out = Vec::new();
    for t in targets {
        let Some(N::ResTarget(rt)) = t.node.as_ref() else {
            continue;
        };
        // A subscripted or field assignment rewrites the stored value.
        if !rt.indirection.is_empty() {
            continue;
        }
        let Some(column) = def.column(&rt.name) else {
            continue;
        };
        let Ok(domain) = column.extra.get_str("domain_type") else {
            continue;
        };
        let Some(val) = rt.val.as_deref() else {
            continue;
        };
        let source = match val.node.as_ref() {
            // `SET (a, b) = (SELECT ...)` / `= (x, y)`: column `colno` of
            // the source.
            Some(N::MultiAssignRef(m)) => {
                let i = usize::try_from(m.colno - 1).unwrap_or(0);
                match m.source.as_deref().and_then(|s| s.node.as_ref()) {
                    Some(N::SubLink(sl)) => sublink_column(sl, i, lookup, ctes),
                    Some(N::RowExpr(r)) => r
                        .args
                        .get(i)
                        .and_then(|a| expr_domain(a, scope, lookup, ctes)),
                    _ => None,
                }
            }
            _ => expr_domain(val, scope, lookup, ctes),
        };
        if is_domain(source.as_deref(), domain) {
            out.push(column.field());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Column as TableColumn;

    fn domain(name: &str, oid: i64) -> domains::Domain {
        domains::Domain {
            name: name.to_string(),
            oid,
            base: "int4".to_string(),
            typmod: -1,
            not_null: false,
            checks: vec![("pos".to_string(), "VALUE > 0".to_string())],
            not_valid: vec!["pos".to_string()],
            default_sql: None,
        }
    }

    fn table() -> TableDef {
        let mut a = TableColumn::new("a", "int4", false);
        a.extra.insert("domain_type", "dnv");
        let mut b = TableColumn::new("b", "int4", false);
        b.extra.insert("domain_type", "dother");
        TableDef::new("fd", vec![a, b, TableColumn::new("i", "int4", false)])
    }

    fn of(sql: &str) -> WriteDomains {
        domains::set_user_domains(vec![domain("dnv", 90031), domain("dother", 90032)]);
        let lookup = |name: &str| (name == "fd").then(table);
        let parsed = pg_query::parse(sql).expect("parses");
        let node = parsed.protobuf.stmts[0].stmt.clone().expect("a statement");
        of_write(&node, &lookup).expect("a write to a table with a domain column")
    }

    /// An UPDATE's value already of its column's domain is no coercion
    /// (RP-664): the column itself (through a FROM item, a subquery, a WITH
    /// too), a COALESCE / GREATEST / CASE of it alone, a cast to the domain.
    /// Anything computed, another domain's column, or a CASE with no ELSE
    /// (its NULL default is untyped) is coerced, and checked.
    #[test]
    fn an_update_value_of_the_column_domain_is_no_coercion() {
        for (sql, expected) in [
            ("UPDATE fd SET a = a", &["a"][..]),
            ("UPDATE fd x SET a = x.a", &["a"]),
            ("UPDATE fd SET a = (-1)::dnv", &["a"]),
            (
                "UPDATE fd SET a = coalesce(a, a), b = greatest(b, b)",
                &["a", "b"],
            ),
            (
                "UPDATE fd SET a = case when i > 0 then a else a end",
                &["a"],
            ),
            ("UPDATE fd SET (a, i) = (SELECT a, 0 FROM fd)", &["a"]),
            ("UPDATE fd SET (a, i) = (a, 0)", &["a"]),
            ("UPDATE fd SET a = t.a FROM fd t", &["a"]),
            ("UPDATE fd SET a = x.z FROM (SELECT a FROM fd) x(z)", &["a"]),
            (
                "WITH q AS (SELECT a FROM fd) UPDATE fd SET a = (SELECT a FROM q)",
                &["a"],
            ),
            ("UPDATE fd SET a = b", &[]),
            ("UPDATE fd SET a = a + 0", &[]),
            ("UPDATE fd SET a = a::int4", &[]),
            ("UPDATE fd SET a = coalesce(a, 1)", &[]),
            ("UPDATE fd SET a = case when i > 0 then a end", &[]),
            ("UPDATE fd SET i = a", &[]),
            // Two FROM items with an `a`: the name is ambiguous, not resolved.
            ("UPDATE fd SET a = (SELECT a FROM fd x, fd y)", &[]),
        ] {
            assert_eq!(of(sql).update, expected, "{sql}");
        }
    }

    /// An INSERT ... SELECT source's column keeps its domain through a
    /// table, a FROM subquery, a WITH and a set operation whose arms agree;
    /// a `*` is expanded only where every FROM item's columns are known.
    #[test]
    fn an_insert_source_column_keeps_its_domain() {
        let dnv = Some("dnv".to_string());
        let dother = Some("dother".to_string());
        for (sql, expected) in [
            (
                "INSERT INTO fd SELECT a, b, i FROM fd",
                vec![dnv.clone(), dother.clone(), None],
            ),
            (
                "INSERT INTO fd SELECT * FROM fd",
                vec![dnv.clone(), dother.clone(), None],
            ),
            (
                "WITH q(z) AS (SELECT a FROM fd) INSERT INTO fd SELECT z, z, 1 FROM q",
                vec![dnv.clone(), dnv.clone(), None],
            ),
            (
                "INSERT INTO fd SELECT a, b, 1 FROM fd UNION ALL SELECT a, a, 1 FROM fd",
                vec![dnv.clone(), None, None],
            ),
            (
                "INSERT INTO fd SELECT * FROM fd JOIN fd x USING (i)",
                vec![],
            ),
            (
                "INSERT INTO fd SELECT * FROM fd, generate_series(1, 2)",
                vec![],
            ),
        ] {
            assert_eq!(of(sql).source, expected, "{sql}");
        }
        assert_eq!(
            of("INSERT INTO fd VALUES (1) ON CONFLICT (a) DO UPDATE SET a = excluded.a, b = fd.a")
                .conflict,
            ["a"]
        );
    }
}
