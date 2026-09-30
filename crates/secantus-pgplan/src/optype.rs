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
/// A CTE in scope: its name and, when it is a plain SELECT, its query.
type Cte = (String, Option<pg_query::protobuf::SelectStmt>);

struct Scope<'p> {
    tables: Vec<(String, TableDef)>,
    complete: bool,
    parent: Option<&'p Scope<'p>>,
}

impl<'p> Scope<'p> {
    fn new(
        items: &[pg_query::protobuf::Node],
        ctes: &[Cte],
        lookup: &dyn Fn(&str) -> Option<TableDef>,
        parent: Option<&'p Scope<'p>>,
    ) -> Scope<'p> {
        let mut scope = Scope {
            tables: Vec::new(),
            complete: true,
            parent,
        };
        for item in items {
            scope.add(item, ctes, lookup);
        }
        scope
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
                let colnames = r
                    .alias
                    .as_ref()
                    .map(|a| a.colnames.clone())
                    .unwrap_or_default();
                match def {
                    Some(q) => self.add_derived(alias, &colnames, q, ctes, lookup),
                    None => self.complete = false,
                }
            }
            Some(N::RangeSubselect(rs)) if !rs.lateral => {
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
                    Some(N::SelectStmt(q)) if !alias.is_empty() => {
                        self.add_derived(alias, &colnames, q, ctes, lookup)
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
                        self.tables.push((alias, def));
                    }
                    None => self.complete = false,
                }
            }
            Some(N::JoinExpr(j)) => {
                // A USING / NATURAL join merges columns; their types are the
                // sides' own, which is what an unqualified lookup finds.
                if let Some(l) = j.larg.as_deref() {
                    self.add(l, ctes, lookup);
                }
                if let Some(r) = j.rarg.as_deref() {
                    self.add(r, ctes, lookup);
                }
            }
            _ => self.complete = false,
        }
    }

    /// A derived table -- a FROM subquery or a CTE -- whose columns are
    /// typed from its select list. A column this cannot type is still a
    /// column (so an unqualified name resolves among the known ones); it is
    /// just never judged.
    fn add_derived(
        &mut self,
        alias: String,
        colnames: &[pg_query::protobuf::Node],
        q: &pg_query::protobuf::SelectStmt,
        ctes: &[Cte],
        lookup: &dyn Fn(&str) -> Option<TableDef>,
    ) {
        if q.op != pg_query::protobuf::SetOperation::SetopNone as i32 || !q.values_lists.is_empty()
        {
            self.complete = false;
            return;
        }
        let inner = Scope::new(&q.from_clause, ctes, lookup, None);
        let mut columns = Vec::new();
        for (i, t) in q.target_list.iter().enumerate() {
            let Some(N::ResTarget(rt)) = t.node.as_ref() else {
                self.complete = false;
                return;
            };
            let Some(val) = rt.val.as_deref() else {
                self.complete = false;
                return;
            };
            // `*` expands to columns this does not list.
            if let Some(N::ColumnRef(c)) = val.node.as_ref() {
                if c.fields.iter().any(|f| matches!(f.node, Some(N::AStar(_)))) {
                    self.complete = false;
                    return;
                }
            }
            let named = |n: &pg_query::protobuf::Node| match n.node.as_ref() {
                Some(N::String(s)) => Some(s.sval.clone()),
                _ => None,
            };
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
            let ty = operand_type(val, &inner).unwrap_or_default();
            columns.push(Column::new(&name, &ty, true));
        }
        self.tables
            .push((alias.clone(), TableDef::new(&alias, columns)));
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
                let mut found = self
                    .tables
                    .iter()
                    .filter_map(|(_, d)| d.column(col).map(|c| c.pg_type.clone()));
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
                None if self.complete => self.parent?.column_type(c),
                None => None,
            },
            _ => None,
        }
    }
}

/// The type an operand has statically, or `None`.
fn operand_type(n: &pg_query::protobuf::Node, scope: &Scope) -> Option<String> {
    use pg_query::protobuf::a_const::Val;
    match n.node.as_ref()? {
        N::AConst(c) if !c.isnull => match c.val.as_ref()? {
            Val::Ival(_) => Some("int4".into()),
            Val::Fval(_) => Some("numeric".into()),
            Val::Boolval(_) => Some("bool".into()),
            _ => None,
        },
        N::TypeCast(tc) => {
            let t = type_name_of(tc.type_name.as_ref()?);
            (!t.ends_with("[]")).then_some(t)
        }
        N::ColumnRef(c) => scope.column_type(c),
        N::ParamRef(p) => declared_param_type(usize::try_from(p.number).ok()?),
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
            (category(&l) == Some("numeric") && category(&r) == Some("numeric")).then(|| {
                if l == r {
                    l
                } else {
                    "numeric".to_string()
                }
            })
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

fn mismatch(op: &str, l: &str, r: &str, location: i32) -> Error {
    set_error_location(location);
    Error::UndefinedFunction(format!(
        "operator does not exist: {} {op} {}",
        display_type(l),
        display_type(r)
    ))
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
    // A COMPOSITE has only the record operators, and those only against the
    // same type: beside any other typed operand there is no operator at all
    // (`integer + point_t`, `point_t = integer`), and a user cast never
    // supplies one.
    if kind == Some(K::AexprOp) {
        let side = |n: Option<&pg_query::protobuf::Node>| n.and_then(|n| operand_type(n, scope));
        if let (Some(l), Some(r)) = (side(e.lexpr.as_deref()), side(e.rexpr.as_deref())) {
            let composite = |t: &str| user_composite(t).is_some();
            if (composite(&l) || composite(&r))
                && user_casts::type_oid(&l) != user_casts::type_oid(&r)
                && l != "record"
                && r != "record"
            {
                let op = if op == "!=" { "<>" } else { op.as_str() };
                return Err(mismatch(op, &l, &r, e.location));
            }
        }
    }
    let Some(l) = e.lexpr.as_deref().and_then(|n| operand_type(n, scope)) else {
        return Ok(());
    };
    let Some(lc) = category(&l) else {
        return Ok(());
    };
    match kind {
        Some(K::AexprOp) if matches!(op.as_str(), "=" | "<>" | "!=" | "<" | "<=" | ">" | ">=") => {
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
            for item in &items.items {
                if let Some(r) = operand_type(item, scope) {
                    if category(&r).is_some_and(|rc| rc != lc) {
                        return Err(mismatch(&op, &l, &r, e.location));
                    }
                }
            }
        }
        Some(K::AexprLike | K::AexprIlike) | Some(K::AexprOp)
            if matches!(op.as_str(), "~~" | "~~*" | "!~~" | "!~~*") =>
        {
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
    let Some(node) = n.node.as_ref() else {
        return Ok(());
    };
    match node {
        N::AExpr(e) => {
            check_aexpr(e, scope)?;
            for s in e.lexpr.iter().chain(e.rexpr.iter()) {
                walk(s, scope, cx)?;
            }
        }
        N::BoolExpr(b) => {
            for a in &b.args {
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
        N::ResTarget(r) => {
            if let Some(v) = r.val.as_deref() {
                walk(v, scope, cx)?;
            }
        }
        // An untyped literal among typed arguments is coerced to their type
        // when the statement is analysed, so a literal that is not valid
        // input for it fails then -- evaluated or not (`coalesce(id, 'x')`).
        N::CoalesceExpr(c) => {
            literals_fit(&c.args, scope)?;
            for a in &c.args {
                walk(a, scope, cx)?;
            }
        }
        N::MinMaxExpr(m) => {
            literals_fit(&m.args, scope)?;
            for a in &m.args {
                walk(a, scope, cx)?;
            }
        }
        N::CaseExpr(c) => {
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
            for s in j.larg.iter().chain(j.rarg.iter()).chain(j.quals.iter()) {
                walk(s, scope, cx)?;
            }
        }
        N::SubLink(sl) => {
            if let Some(t) = sl.testexpr.as_deref() {
                walk(t, scope, cx)?;
            }
            if let Some(N::SelectStmt(sel)) = sl.subselect.as_deref().and_then(|n| n.node.as_ref())
            {
                check_select(sel, &cx.ctes, cx.lookup, Some(scope))?;
            }
        }
        N::FuncCall(f) => {
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
        }
        // A FROM subquery sees the outer query only when it is LATERAL.
        N::RangeSubselect(rs) => {
            if let Some(N::SelectStmt(sel)) = rs.subquery.as_deref().and_then(|n| n.node.as_ref()) {
                check_select(sel, &cx.ctes, cx.lookup, rs.lateral.then_some(scope))?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_select(
    s: &pg_query::protobuf::SelectStmt,
    outer_ctes: &[Cte],
    lookup: &dyn Fn(&str) -> Option<TableDef>,
    parent: Option<&Scope>,
) -> Result<()> {
    let mut ctes = outer_ctes.to_vec();
    if let Some(w) = &s.with_clause {
        for c in &w.ctes {
            if let Some(N::CommonTableExpr(c)) = c.node.as_ref() {
                let query = match c.ctequery.as_deref().and_then(|n| n.node.as_ref()) {
                    Some(N::SelectStmt(q)) if !w.recursive => Some((**q).clone()),
                    _ => None,
                };
                ctes.push((c.ctename.clone(), query));
            }
        }
    }
    // A set operation's arms each have their own FROM, under the same parent.
    if let Some(l) = s.larg.as_deref() {
        check_select(l, &ctes, lookup, parent)?;
    }
    if let Some(r) = s.rarg.as_deref() {
        check_select(r, &ctes, lookup, parent)?;
    }
    let scope = Scope::new(&s.from_clause, &ctes, lookup, parent);
    let cx = Cx { lookup, ctes };
    for f in &s.from_clause {
        walk(f, &scope, &cx)?;
    }
    for t in &s.target_list {
        walk(t, &scope, &cx)?;
    }
    if let Some(w) = s.where_clause.as_deref() {
        walk(w, &scope, &cx)?;
    }
    Ok(())
}

/// 42883 for a comparison with no operator between its operands' types.
pub(crate) fn check(node: &N, lookup: &dyn Fn(&str) -> Option<TableDef>) -> Result<()> {
    let relation = |r: &Option<pg_query::protobuf::RangeVar>| {
        r.as_ref().map(|r| pg_query::protobuf::Node {
            node: Some(N::RangeVar(r.clone())),
        })
    };
    match node {
        N::SelectStmt(s) => check_select(s, &[], lookup, None),
        N::UpdateStmt(u) if u.with_clause.is_none() => {
            let items: Vec<_> = relation(&u.relation)
                .into_iter()
                .chain(u.from_clause.iter().cloned())
                .collect();
            let scope = Scope::new(&items, &[], lookup, None);
            let cx = Cx {
                lookup,
                ctes: Vec::new(),
            };
            match u.where_clause.as_deref() {
                Some(w) => walk(w, &scope, &cx),
                None => Ok(()),
            }
        }
        N::DeleteStmt(d) if d.with_clause.is_none() => {
            let items: Vec<_> = relation(&d.relation)
                .into_iter()
                .chain(d.using_clause.iter().cloned())
                .collect();
            let scope = Scope::new(&items, &[], lookup, None);
            let cx = Cx {
                lookup,
                ctes: Vec::new(),
            };
            match d.where_clause.as_deref() {
                Some(w) => walk(w, &scope, &cx),
                None => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

/// Do the untyped string literals among `args` read as the type the typed
/// ones share? The first literal that does not is its cast's error, at the
/// literal.
fn literals_fit(args: &[pg_query::protobuf::Node], scope: &Scope) -> Result<()> {
    use pg_query::protobuf::a_const::Val;
    let typed: Vec<String> = args.iter().filter_map(|a| operand_type(a, scope)).collect();
    let Some(first) = typed.first() else {
        return Ok(());
    };
    if typed.iter().any(|t| t != first) {
        return Ok(());
    }
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
