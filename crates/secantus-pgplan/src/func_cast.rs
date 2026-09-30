//! Two things PostgreSQL's LEXER and parse analysis decide that the grammar
//! this server parses with (PostgreSQL 17's) decides differently:
//!
//! * `typename(expr)` -- a one-argument call named like a type, with no
//!   function of that name -- is a CAST (`jsonb('{}')`, `inet('1.2.3.4')`,
//!   `int8(3)`). PostgreSQL 15 reads `json(x)` that way too, where the 17
//!   grammar builds a `JSON()` constructor; both are rewritten here into the
//!   `TypeCast` the rest of the planner already handles.
//! * PostgreSQL 15's lexer refuses a numeric literal running straight into an
//!   identifier character -- `1_000`, `0x10`, `1.5e1_0` -- as "trailing junk
//!   after numeric literal"; the 16+ grammar reads those as numbers.

use super::*;

/// The numeric-literal "trailing junk" check, over the SQL text: strings,
/// quoted identifiers, dollar quotes, comments and `$n` parameters skipped.
pub(crate) fn check_numeric_junk(sql: &str) -> Result<()> {
    let b: Vec<char> = sql.chars().collect();
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$' || (c as u32) > 127;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            '\'' | '"' => {
                // `E'...'` takes backslash escapes, so `\'` does not end it.
                let escapes = c == '\''
                    && i > 0
                    && matches!(b[i - 1], 'e' | 'E')
                    && (i < 2 || !ident(b[i - 2]));
                i += 1;
                while i < b.len() {
                    if escapes && b[i] == '\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == c {
                        if i + 1 < b.len() && b[i + 1] == c {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                i += 1;
            }
            '-' if b.get(i + 1) == Some(&'-') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if b.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            '$' => {
                // `$n`, or a dollar quote `$tag$ ... $tag$`.
                let mut j = i + 1;
                while j < b.len() && (b[j].is_alphanumeric() || b[j] == '_') {
                    j += 1;
                }
                if j < b.len()
                    && b[j] == '$'
                    && !b[i + 1..j].first().is_some_and(char::is_ascii_digit)
                {
                    let tag: String = b[i..=j].iter().collect();
                    let rest: String = b[j + 1..].iter().collect();
                    let end = rest.find(&tag).map_or(b.len(), |p| {
                        j + 1 + rest[..p].chars().count() + tag.chars().count()
                    });
                    i = end;
                } else {
                    i = j;
                }
            }
            d if d.is_ascii_digit() && (i == 0 || !(ident(b[i - 1]) || b[i - 1] == '.')) => {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i < b.len() && b[i] == '.' && b.get(i + 1) != Some(&'.') {
                    i += 1;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
                    let mut j = i + 1;
                    if j < b.len() && (b[j] == '+' || b[j] == '-') {
                        j += 1;
                    }
                    if j < b.len() && b[j].is_ascii_digit() {
                        i = j;
                        while i < b.len() && b[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                if i < b.len() && (b[i].is_alphabetic() || b[i] == '_') {
                    let mut end = i;
                    while end < b.len() && ident(b[end]) {
                        end += 1;
                    }
                    let token: String = b[start..end].iter().collect();
                    return Err(Error::Parse(format!(
                        "trailing junk after numeric literal at or near \"{token}\""
                    )));
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}

/// The type a `name(arg)` call casts to, when it is a function-style cast:
/// one plain argument, a name that is a type, and no function of that name.
fn cast_target(f: &pg_query::protobuf::FuncCall) -> Option<pg_query::protobuf::TypeName> {
    if f.args.len() != 1
        || f.agg_star
        || f.agg_distinct
        || f.func_variadic
        || f.over.is_some()
        || f.agg_filter.is_some()
        || !f.agg_order.is_empty()
        || matches!(f.args[0].node, Some(N::NamedArgExpr(_)))
    {
        return None;
    }
    let names: Vec<&str> = f
        .funcname
        .iter()
        .filter_map(|n| match n.node.as_ref() {
            Some(N::String(s)) => Some(s.sval.as_str()),
            _ => None,
        })
        .collect();
    let name = match names.as_slice() {
        [n] | ["pg_catalog", n] => *n,
        _ => return None,
    };
    if scalar::is_scalar(name)
        || correlated::user_function_named(name)
        || aggregate_func(name, false).is_some()
        || !column_type_exists(name)
    {
        return None;
    }
    Some(pg_query::protobuf::TypeName {
        names: f.funcname.clone(),
        typemod: -1,
        location: f.location,
        ..Default::default()
    })
}

/// The replacement for `n`, when it is a function-style cast or `JSON(x)`.
fn as_cast(n: &pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    let (arg, type_name) = match n.node.as_ref()? {
        N::FuncCall(f) => (f.args[..].first()?.clone(), cast_target(f)?),
        N::JsonParseExpr(p) => {
            let arg = p.expr.as_ref()?.raw_expr.as_deref()?.clone();
            let json = |s: &str| pg_query::protobuf::Node {
                node: Some(N::String(pg_query::protobuf::String { sval: s.into() })),
            };
            (
                arg,
                pg_query::protobuf::TypeName {
                    names: vec![json("pg_catalog"), json("json")],
                    typemod: -1,
                    location: p.location,
                    ..Default::default()
                },
            )
        }
        _ => return None,
    };
    Some(pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(arg)),
            type_name: Some(type_name),
            location: -1,
        }))),
    })
}

/// Replace one child slot, when it holds a cast in call form.
fn fix(slot: &mut pg_query::protobuf::Node) -> bool {
    match user_ops::replacement(slot).or_else(|| as_cast(slot)) {
        Some(cast) => {
            *slot = cast;
            true
        }
        None => false,
    }
}

fn fix_box(slot: &mut Option<Box<pg_query::protobuf::Node>>) -> bool {
    slot.as_deref_mut().is_some_and(fix)
}

fn fix_all(slots: &mut [pg_query::protobuf::Node]) -> bool {
    slots.iter_mut().any(fix)
}

/// Rewrite every function-style cast in `node` into a `TypeCast`.
///
/// One replacement per pass over a fresh node list: replacing a node frees
/// what the list's later pointers may point into, so the walk restarts
/// rather than touch them.
pub(crate) fn rewrite(node: &mut N) {
    let wanted = user_ops::wanted(node)
        || node.nodes().iter().any(|(n, _, _, _)| match n {
            pg_query::NodeRef::FuncCall(f) => cast_target(f).is_some(),
            pg_query::NodeRef::JsonParseExpr(_) => true,
            _ => false,
        });
    if !wanted {
        return;
    }
    // The statement itself may be the call's container only through a
    // child slot, so the root never needs replacing.
    for _ in 0..10_000 {
        let mut replaced = false;
        // SAFETY: each pointer is used only until the first replacement,
        // after which the loop restarts with a fresh list.
        unsafe {
            for (n, _, _) in node.nodes_mut() {
                use pg_query::NodeMut as M;
                replaced = match n {
                    M::ResTarget(r) => fix_box(&mut (*r).val),
                    M::AExpr(e) => fix_box(&mut (*e).lexpr) || fix_box(&mut (*e).rexpr),
                    M::BoolExpr(e) => fix_all(&mut (*e).args),
                    M::FuncCall(f) => fix_all(&mut (*f).args),
                    M::TypeCast(t) => fix_box(&mut (*t).arg),
                    M::List(l) => fix_all(&mut (*l).items),
                    M::CaseExpr(c) => {
                        fix_box(&mut (*c).arg)
                            || fix_all(&mut (*c).args)
                            || fix_box(&mut (*c).defresult)
                    }
                    M::CaseWhen(w) => fix_box(&mut (*w).expr) || fix_box(&mut (*w).result),
                    M::CoalesceExpr(c) => fix_all(&mut (*c).args),
                    M::MinMaxExpr(m) => fix_all(&mut (*m).args),
                    M::NullTest(t) => fix_box(&mut (*t).arg),
                    M::BooleanTest(t) => fix_box(&mut (*t).arg),
                    M::RowExpr(r) => fix_all(&mut (*r).args),
                    M::AArrayExpr(a) => fix_all(&mut (*a).elements),
                    M::AIndirection(a) => fix_box(&mut (*a).arg),
                    M::SubLink(s) => fix_box(&mut (*s).testexpr),
                    M::SortBy(s) => fix_box(&mut (*s).node),
                    M::SelectStmt(s) => {
                        fix_box(&mut (*s).where_clause)
                            || fix_box(&mut (*s).having_clause)
                            || fix_all(&mut (*s).group_clause)
                    }
                    M::UpdateStmt(u) => fix_box(&mut (*u).where_clause),
                    M::DeleteStmt(d) => fix_box(&mut (*d).where_clause),
                    M::JoinExpr(j) => fix_box(&mut (*j).quals),
                    M::JsonValueExpr(v) => fix_box(&mut (*v).raw_expr),
                    _ => false,
                };
                if replaced {
                    break;
                }
            }
        }
        if !replaced {
            return;
        }
    }
}

/// What parse analysis refuses because it cannot type a value: `$1 IS NULL`
/// over a parameter the client left untyped is 42P18, and `to_json` /
/// `to_jsonb` / `array_to_json` of an untyped literal or parameter is 42804
/// (a polymorphic argument must have a type).
pub(crate) fn refuse_unresolved(node: &N) -> Result<()> {
    let untyped_param = |n: &pg_query::protobuf::Node| -> Option<i32> {
        match n.node.as_ref() {
            Some(N::ParamRef(p))
                if declared_param_type(usize::try_from(p.number).unwrap_or(0)).is_none() =>
            {
                Some(p.number)
            }
            _ => None,
        }
    };
    let untyped_literal = |n: &pg_query::protobuf::Node| {
        matches!(n.node.as_ref(), Some(N::AConst(c))
            if matches!(c.val, Some(pg_query::protobuf::a_const::Val::Sval(_))))
    };
    for (n, _, _, _) in node.nodes() {
        match n {
            pg_query::NodeRef::NullTest(t) => {
                if let Some(p) = t.arg.as_deref().and_then(untyped_param) {
                    return Err(Error::Sqlstate(
                        "42P18",
                        format!("could not determine data type of parameter ${p}"),
                    ));
                }
            }
            pg_query::NodeRef::FuncCall(f)
                if matches!(
                    func_name(f).as_deref(),
                    Some("to_json" | "to_jsonb" | "array_to_json")
                ) && !correlated::user_function_named(&func_name(f).unwrap_or_default()) =>
            {
                if f.args
                    .first()
                    .is_some_and(|a| untyped_literal(a) || untyped_param(a).is_some())
                {
                    return Err(Error::DatatypeMismatch(
                        "could not determine polymorphic type because input has type unknown"
                            .into(),
                    ));
                }
            }
            _ => {}
        }
    }
    Ok(())
}
