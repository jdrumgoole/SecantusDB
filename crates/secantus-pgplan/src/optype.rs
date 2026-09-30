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

/// The catalog: a relation's definition by name.
type Lookup<'p> = &'p dyn Fn(&str) -> Option<TableDef>;

struct Scope<'p> {
    tables: Vec<(String, TableDef)>,
    complete: bool,
    parent: Option<&'p Scope<'p>>,
    /// The catalog and CTEs in view, to type a scalar subquery's column.
    lookup: Option<Lookup<'p>>,
    ctes: Vec<Cte>,
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
            parent,
            lookup: Some(lookup),
            ctes: ctes.to_vec(),
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
        // A scalar subquery: its one column's type, in a scope of its own.
        N::SubLink(sl)
            if pg_query::protobuf::SubLinkType::try_from(sl.sub_link_type)
                == Ok(pg_query::protobuf::SubLinkType::ExprSublink) =>
        {
            let Some(N::SelectStmt(sub)) = sl.subselect.as_deref().and_then(|n| n.node.as_ref())
            else {
                return None;
            };
            if sub.op != pg_query::protobuf::SetOperation::SetopNone as i32
                || sub.with_clause.is_some()
            {
                return None;
            }
            let lookup = scope.lookup?;
            let inner = Scope::new(&sub.from_clause, &scope.ctes, lookup, Some(scope));
            let Some(N::ResTarget(rt)) = sub.target_list.first()?.node.as_ref() else {
                return None;
            };
            operand_type(rt.val.as_deref()?, &inner)
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
                    Some(if l == r {
                        l.to_string()
                    } else {
                        "numeric".to_string()
                    })
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
    let Some(lc) = category(&l) else {
        return Ok(());
    };
    match kind {
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
            Some(o) if o == t => o,
            Some(o) if category(&o) == Some("numeric") && category(&t) == Some("numeric") => {
                "numeric".into()
            }
            Some(o) if category(&o) == Some("string") && category(&t) == Some("string") => {
                "text".into()
            }
            _ => return None,
        });
    }
    out
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
