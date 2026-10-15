//! PostgreSQL's built-in functions that are another expression under a
//! second name, rewritten into that expression before planning.
//!
//! Every operator is a function too: `1 + 2` runs `int4pl(1, 2)`, and
//! `pg_proc` lets a statement call it by that name. So does a function that
//! exists only so an operator or a cast has something to point at
//! (`textlen` behind `length`, `dexp` behind `exp`). This server implements
//! the operators and the SQL-facing names; a call by the other name is
//! rewritten here into the expression it stands for.
//!
//! `pg_operator_functions.tsv` (function, argument count, left type, right
//! type, operator) was read from PostgreSQL 15.19's `pg_operator`, and holds
//! the functions `tools/probes/pg_builtin_coverage.py` measured this server
//! refusing. A function this server implements under its own name is not in
//! it, and is not rewritten. Neither is one with a VARIADIC overload
//! (`jsonb_extract_path(j, 'a', 'b')`): its two-argument call is the path's
//! first step, where the operator's right side is the whole path.
//!
//! A literal argument is given the function's declared type where PostgreSQL
//! would coerce it (`int8pl(1, 2)` is a `bigint`), and a literal it would NOT
//! coerce leaves the call unrewritten, to be refused as PostgreSQL refuses it
//! (`int4pl(1.5, 2)`, 42883). Any other argument is left as it is: a column's
//! type is not known here, so `int4pl(numeric_col, 2)` still runs.

use super::*;
use std::collections::HashMap;
use std::sync::OnceLock;

const OPERATORS: &str = include_str!("pg_operator_functions.tsv");

/// The functions that are a call or an expression over their arguments:
/// `(name, argument count, the expression, the type each `$n` is cast to)`.
const TEMPLATES: &[(&str, usize, &str)] = &[
    // `jsonb_delete(j, k)` is `j - k`, for a text key, an index, or the
    // VARIADIC array (`func_cast::spread_variadic` names that form).
    ("jsonb_delete", 2, "$1::jsonb - $2"),
    ("\u{1f}variadic_jsonb_delete", 2, "$1::jsonb - $2::text[]"),
    // `jsonb_extract_path(j, VARIADIC path)` is `j #> path`.
    (
        "\u{1f}variadic_jsonb_extract_path",
        2,
        "$1::jsonb #> $2::text[]",
    ),
    (
        "\u{1f}variadic_jsonb_extract_path_text",
        2,
        "$1::jsonb #>> $2::text[]",
    ),
    (
        "\u{1f}variadic_json_extract_path",
        2,
        "$1::json #> $2::text[]",
    ),
    (
        "\u{1f}variadic_json_extract_path_text",
        2,
        "$1::json #>> $2::text[]",
    ),
    ("textlen", 1, "length($1::text)"),
    ("dexp", 1, "exp($1::float8)"),
    ("dlog1", 1, "ln($1::float8)"),
    ("dlog10", 1, "log10($1::float8)"),
    ("dround", 1, "round($1::float8)"),
    ("dtrunc", 1, "trunc($1::float8)"),
    ("dceil", 1, "ceil($1::float8)"),
    ("dfloor", 1, "floor($1::float8)"),
    ("dsqrt", 1, "sqrt($1::float8)"),
    ("dcbrt", 1, "cbrt($1::float8)"),
    ("dpow", 2, "power($1::float8, $2::float8)"),
    ("numeric_exp", 1, "exp($1::numeric)"),
    ("numeric_ln", 1, "ln($1::numeric)"),
    ("numeric_log", 2, "log($1::numeric, $2::numeric)"),
    ("numeric_sqrt", 1, "sqrt($1::numeric)"),
    ("numeric_power", 2, "power($1::numeric, $2::numeric)"),
    ("numeric_abs", 1, "abs($1::numeric)"),
    ("numeric_inc", 1, "$1::numeric + 1"),
    ("numeric_div_trunc", 2, "div($1::numeric, $2::numeric)"),
    ("int4inc", 1, "$1::int4 + 1"),
    ("int2abs", 1, "abs($1::int2)"),
    ("int4abs", 1, "abs($1::int4)"),
    ("int8abs", 1, "abs($1::int8)"),
    ("float4abs", 1, "abs($1::float4)"),
    ("float8abs", 1, "abs($1::float8)"),
    ("getpgusername", 0, "current_user"),
    // `pg_catalog."session_user"()`: the function behind the keyword.
    ("session_user", 0, "session_user"),
    ("like", 2, "$1 LIKE $2"),
    ("notlike", 2, "$1 NOT LIKE $2"),
    (
        "nameconcatoid",
        2,
        "($1::text || '_' || $2::oid::text)::name",
    ),
];

struct Operator {
    left: String,
    right: String,
    symbol: String,
}

fn operators() -> &'static HashMap<(String, usize), Operator> {
    static O: OnceLock<HashMap<(String, usize), Operator>> = OnceLock::new();
    O.get_or_init(|| {
        OPERATORS
            .lines()
            .filter_map(|l| {
                let mut it = l.split('\t');
                let (name, n, left, right, symbol) =
                    (it.next()?, it.next()?, it.next()?, it.next()?, it.next()?);
                Some((
                    (name.to_string(), n.parse().ok()?),
                    Operator {
                        left: left.to_string(),
                        right: right.to_string(),
                        symbol: symbol.to_string(),
                    },
                ))
            })
            .collect()
    })
}

/// PostgreSQL 15.19's `pg_type`: `typname`, `typoutput`, `typsend`,
/// `typmodout` for its base, range and multirange types.
const TYPE_IO: &str = include_str!("pg_type_io.tsv");

/// Each type's output function, as its value's I/O conversion to `cstring`:
/// `int4out(x)` is `x::int4::cstring`. A function several types share
/// (`range_out`, `array_out`) takes its argument as it comes.
fn output_templates() -> Vec<(&'static str, String)> {
    let rows: Vec<(&str, &str)> = TYPE_IO
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some((it.next()?, it.next()?))
        })
        .filter(|(_, out)| *out != "-")
        .collect();
    let shared = |out: &str| rows.iter().filter(|(_, o)| *o == out).count() > 1;
    let mut out: Vec<(&'static str, String)> = rows
        .iter()
        .map(|(ty, f)| {
            // A bare `bit` is `bit(1)` in a cast, and would cut the value.
            let sql = if shared(f) || *ty == "bit" {
                "$1::cstring".to_string()
            } else {
                format!("$1::{}::cstring", scalar::quote_identifier(ty))
            };
            (&*Box::leak(f.to_string().into_boxed_str()), sql)
        })
        .collect();
    out.push(("array_out", "$1::cstring".to_string()));
    out
}

fn templates() -> &'static HashMap<(&'static str, usize), pg_query::protobuf::Node> {
    static T: OnceLock<HashMap<(&'static str, usize), pg_query::protobuf::Node>> = OnceLock::new();
    T.get_or_init(|| {
        let mut map: HashMap<(&'static str, usize), pg_query::protobuf::Node> = TEMPLATES
            .iter()
            .filter_map(|(name, n, sql)| Some(((*name, *n), parse_expression(sql)?)))
            .collect();
        for (name, sql) in output_templates() {
            if let Some(node) = parse_expression(&sql) {
                map.entry((name, 1)).or_insert(node);
            }
        }
        map
    })
}

/// One SQL expression as its parse tree.
fn parse_expression(sql: &str) -> Option<pg_query::protobuf::Node> {
    let parsed = pg_query::parse(&format!("SELECT {sql}")).ok()?;
    let stmt = parsed
        .protobuf
        .stmts
        .first()?
        .stmt
        .as_ref()?
        .node
        .as_ref()?;
    let N::SelectStmt(s) = stmt else {
        return None;
    };
    let Some(N::ResTarget(rt)) = s.target_list.first()?.node.as_ref() else {
        return None;
    };
    Some((**rt.val.as_ref()?).clone())
}

/// The plain call `f` is, as `(name, arguments)`: no aggregate or window
/// decoration, no named or VARIADIC argument, and no user function of the
/// name, which would be found first.
fn plain_call(f: &pg_query::protobuf::FuncCall) -> Option<(String, &[pg_query::protobuf::Node])> {
    if f.agg_star
        || f.agg_distinct
        || f.func_variadic
        || f.over.is_some()
        || f.agg_filter.is_some()
        || !f.agg_order.is_empty()
        || f.args
            .iter()
            .any(|a| matches!(a.node, Some(N::NamedArgExpr(_))))
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
    if correlated::user_function_named(name) {
        return None;
    }
    Some((name.to_string(), &f.args))
}

/// The functions behind `max` / `min` and behind a B-tree's ordering, which
/// exist once per type: `int4larger`, `text_smaller`, `bttextcmp`. Each is
/// strict, so a NULL argument answers NULL, which a CASE with no ELSE gives.
const LARGER: &str = "CASE WHEN $1 >= $2 THEN $1 WHEN $1 < $2 THEN $2 END";
const SMALLER: &str = "CASE WHEN $1 <= $2 THEN $1 WHEN $1 > $2 THEN $2 END";
const COMPARE: &str = "CASE WHEN $1 < $2 THEN -1 WHEN $1 > $2 THEN 1 WHEN $1 = $2 THEN 0 END";

/// PostgreSQL 15's `*larger` / `*smaller` / B-tree `*cmp` functions this
/// server has the comparison for, read from `pg_proc`.
///
/// `bttextcmp`, `bpcharcmp` and `byteacmp` are left out on purpose: each
/// answers what the C library's `memcmp` returns, which is the difference of
/// the first two bytes that differ on one platform (`bttextcmp('b', 'a')` is
/// 14 on macOS) and -1 / 1 on another.
const FAMILIES: &[(&str, &str)] = &[
    ("int2larger", LARGER),
    ("int4larger", LARGER),
    ("int8larger", LARGER),
    ("float4larger", LARGER),
    ("float8larger", LARGER),
    ("numeric_larger", LARGER),
    ("text_larger", LARGER),
    ("bpchar_larger", LARGER),
    ("date_larger", LARGER),
    ("time_larger", LARGER),
    ("timestamp_larger", LARGER),
    ("timestamptz_larger", LARGER),
    ("interval_larger", LARGER),
    ("oidlarger", LARGER),
    ("tidlarger", LARGER),
    ("int2smaller", SMALLER),
    ("int4smaller", SMALLER),
    ("int8smaller", SMALLER),
    ("float4smaller", SMALLER),
    ("float8smaller", SMALLER),
    ("numeric_smaller", SMALLER),
    ("text_smaller", SMALLER),
    ("bpchar_smaller", SMALLER),
    ("date_smaller", SMALLER),
    ("time_smaller", SMALLER),
    ("timestamp_smaller", SMALLER),
    ("timestamptz_smaller", SMALLER),
    ("interval_smaller", SMALLER),
    ("oidsmaller", SMALLER),
    ("tidsmaller", SMALLER),
    ("btint2cmp", COMPARE),
    ("btint4cmp", COMPARE),
    ("btint8cmp", COMPARE),
    ("btint24cmp", COMPARE),
    ("btint42cmp", COMPARE),
    ("btint28cmp", COMPARE),
    ("btint82cmp", COMPARE),
    ("btint48cmp", COMPARE),
    ("btint84cmp", COMPARE),
    ("btfloat4cmp", COMPARE),
    ("btfloat8cmp", COMPARE),
    ("btfloat48cmp", COMPARE),
    ("btfloat84cmp", COMPARE),
    ("numeric_cmp", COMPARE),
    ("btboolcmp", COMPARE),
    ("btoidcmp", COMPARE),
    ("date_cmp", COMPARE),
    ("time_cmp", COMPARE),
    ("timestamp_cmp", COMPARE),
    ("timestamptz_cmp", COMPARE),
    ("interval_cmp", COMPARE),
    ("uuid_cmp", COMPARE),
];

/// The two argument types a `FAMILIES` function declares, read off its
/// name as `pg_proc` spells it: `int4larger(int4, int4)`,
/// `btint48cmp(int4, int8)`, `text_smaller(text, text)`.
fn family_types(name: &str) -> (&'static str, &'static str) {
    let stem = name
        .trim_start_matches("bt")
        .trim_end_matches("larger")
        .trim_end_matches("smaller")
        .trim_end_matches("cmp")
        .trim_end_matches('_');
    let one = |t: &str| -> &'static str {
        match t {
            "int2" | "2" => "int2",
            "int4" | "4" => "int4",
            "int8" | "8" => "int8",
            "float4" => "float4",
            "float8" => "float8",
            "numeric" => "numeric",
            "text" => "text",
            "bpchar" => "bpchar",
            "bool" => "bool",
            "oid" => "oid",
            "date" => "date",
            "time" => "time",
            "timestamp" => "timestamp",
            "timestamptz" => "timestamptz",
            "interval" => "interval",
            "uuid" => "uuid",
            "tid" => "tid",
            _ => "",
        }
    };
    // `int48` / `float84`: one digit for each side.
    for family in ["int", "float"] {
        if let Some(digits) = stem.strip_prefix(family) {
            if digits.len() == 2 && digits.chars().all(|c| c.is_ascii_digit()) {
                let side = |d: &str| -> &'static str {
                    match (family, d) {
                        ("int", "2") => "int2",
                        ("int", "4") => "int4",
                        ("int", "8") => "int8",
                        ("float", "4") => "float4",
                        ("float", "8") => "float8",
                        _ => "",
                    }
                };
                return (side(&digits[..1]), side(&digits[1..]));
            }
        }
    }
    (one(stem), one(stem))
}

fn family_template(name: &str, nargs: usize) -> Option<&'static pg_query::protobuf::Node> {
    static T: OnceLock<HashMap<&'static str, pg_query::protobuf::Node>> = OnceLock::new();
    if nargs != 2 {
        return None;
    }
    T.get_or_init(|| {
        FAMILIES
            .iter()
            .filter_map(|(name, sql)| Some((*name, parse_expression(sql)?)))
            .collect()
    })
    .get(name)
}

/// Is `f` a call this module rewrites?
pub(crate) fn wanted(f: &pg_query::protobuf::FuncCall) -> bool {
    plain_call(f).is_some_and(|(name, args)| {
        operators().contains_key(&(name.clone(), args.len()))
            || templates().contains_key(&(name.as_str(), args.len()))
            || family_template(&name, args.len()).is_some()
    })
}

/// Does the literal `n` take the declared type `ty` on PostgreSQL, where a
/// function's parameter coerces its argument? A string literal is untyped
/// and takes any; an integer literal has an implicit cast to every numeric
/// type; a literal with a fraction is `numeric`, which casts implicitly to
/// the float types only.
fn literal_takes(n: &pg_query::protobuf::Node, ty: &str) -> bool {
    use pg_query::protobuf::a_const::Val;
    let Some(N::AConst(c)) = n.node.as_ref() else {
        return false;
    };
    match c.val {
        Some(Val::Sval(_)) => true,
        Some(Val::Ival(_)) => matches!(
            ty,
            "int2" | "int4" | "int8" | "float4" | "float8" | "numeric" | "oid" | "money"
        ),
        Some(Val::Fval(_)) => matches!(ty, "float4" | "float8" | "numeric"),
        _ => false,
    }
}

/// Is the literal `n` one PostgreSQL would NOT pass to a parameter of type
/// `ty`? Then the function does not exist for these arguments (42883), and
/// the call is left for the planner to refuse with the argument types -- the
/// rewrite used to run it anyway (`int4pl(1.5, 2)` answered 3.5). An
/// untyped string or a NULL takes any type; a whole number past int4 is a
/// `bigint` (pg_query reads it as a float); a polymorphic or unknown `ty`
/// refuses nothing.
fn literal_refused(n: &pg_query::protobuf::Node, ty: &str) -> bool {
    use pg_query::protobuf::a_const::Val;
    if ty.is_empty() || ty.starts_with("any") || ty.starts_with('_') {
        return false;
    }
    let Some(N::AConst(c)) = n.node.as_ref() else {
        return false;
    };
    if c.isnull {
        return false;
    }
    match &c.val {
        Some(Val::Sval(_)) => false,
        Some(Val::Ival(_)) => !literal_takes(n, ty),
        Some(Val::Fval(f)) if f.fval.bytes().all(|b| b.is_ascii_digit() || b == b'-') => {
            let wide = if f.fval.parse::<i64>().is_ok() {
                "int8"
            } else {
                "numeric"
            };
            !matches!(ty, "float4" | "float8" | "numeric") && !(wide == "int8" && ty == "int8")
        }
        Some(Val::Fval(_)) => !literal_takes(n, ty),
        Some(Val::Boolval(_)) => !matches!(ty, "bool" | "boolean"),
        _ => false,
    }
}

/// The type each `$n` of a template is cast to (`length($1::text)`), or
/// "" where the template leaves it uncast.
fn template_param_types(template: &pg_query::protobuf::Node, nargs: usize) -> Vec<String> {
    fn walk(n: &pg_query::protobuf::Node, out: &mut Vec<String>) {
        match n.node.as_ref() {
            Some(N::TypeCast(t)) => {
                if let (Some(N::ParamRef(p)), Some(tn)) = (
                    t.arg.as_deref().and_then(|a| a.node.as_ref()),
                    t.type_name.as_ref(),
                ) {
                    let i = usize::try_from(p.number - 1).unwrap_or(usize::MAX);
                    if let (Some(slot), Some(Some(N::String(name)))) =
                        (out.get_mut(i), tn.names.last().map(|n| n.node.as_ref()))
                    {
                        *slot = name.sval.clone();
                    }
                }
                if let Some(a) = t.arg.as_deref() {
                    walk(a, out);
                }
            }
            Some(N::AExpr(e)) => {
                e.lexpr
                    .as_deref()
                    .into_iter()
                    .chain(e.rexpr.as_deref())
                    .for_each(|n| walk(n, out));
            }
            Some(N::FuncCall(f)) => f.args.iter().for_each(|n| walk(n, out)),
            Some(N::CaseExpr(c)) => {
                c.arg
                    .as_deref()
                    .into_iter()
                    .chain(c.defresult.as_deref())
                    .for_each(|n| walk(n, out));
                c.args.iter().for_each(|n| walk(n, out));
            }
            Some(N::CaseWhen(w)) => {
                w.expr
                    .as_deref()
                    .into_iter()
                    .chain(w.result.as_deref())
                    .for_each(|n| walk(n, out));
            }
            _ => {}
        }
    }
    let mut out = vec![String::new(); nargs];
    walk(template, &mut out);
    out
}

fn string(s: &str) -> pg_query::protobuf::Node {
    pg_query::protobuf::Node {
        node: Some(N::String(pg_query::protobuf::String { sval: s.into() })),
    }
}

/// `arg`, cast to `ty` when it is a literal the parameter would coerce and
/// `ty` is a real type.
fn typed(arg: &pg_query::protobuf::Node, ty: &str) -> pg_query::protobuf::Node {
    // An array type is spelled `_elem` in the catalog; its literal is left
    // for the operator to type.
    if ty.is_empty() || ty.starts_with("any") || ty.starts_with('_') || !literal_takes(arg, ty) {
        return arg.clone();
    }
    pg_query::protobuf::Node {
        node: Some(N::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(arg.clone())),
            type_name: Some(pg_query::protobuf::TypeName {
                names: vec![string("pg_catalog"), string(ty)],
                typemod: -1,
                location: -1,
                ..Default::default()
            }),
            location: -1,
        }))),
    }
}

/// `template` with each `$n` replaced by `args[n - 1]`.
fn substitute(
    template: &pg_query::protobuf::Node,
    args: &[pg_query::protobuf::Node],
) -> pg_query::protobuf::Node {
    let sub = |n: &Option<Box<pg_query::protobuf::Node>>| {
        n.as_deref().map(|n| Box::new(substitute(n, args)))
    };
    let all = |ns: &[pg_query::protobuf::Node]| ns.iter().map(|n| substitute(n, args)).collect();
    let node = match template.node.as_ref() {
        Some(N::ParamRef(p)) => {
            return usize::try_from(p.number - 1)
                .ok()
                .and_then(|i| args.get(i))
                .cloned()
                .unwrap_or_else(|| template.clone());
        }
        Some(N::AExpr(e)) => {
            let mut out = (**e).clone();
            out.lexpr = sub(&e.lexpr);
            out.rexpr = sub(&e.rexpr);
            N::AExpr(Box::new(out))
        }
        Some(N::FuncCall(f)) => {
            let mut out = (**f).clone();
            out.args = all(&f.args);
            N::FuncCall(Box::new(out))
        }
        Some(N::TypeCast(t)) => {
            let mut out = (**t).clone();
            out.arg = sub(&t.arg);
            N::TypeCast(Box::new(out))
        }
        Some(N::CaseExpr(c)) => {
            let mut out = (**c).clone();
            out.arg = sub(&c.arg);
            out.args = all(&c.args);
            out.defresult = sub(&c.defresult);
            N::CaseExpr(Box::new(out))
        }
        Some(N::CaseWhen(w)) => {
            let mut out = (**w).clone();
            out.expr = sub(&w.expr);
            out.result = sub(&w.result);
            N::CaseWhen(Box::new(out))
        }
        _ => return template.clone(),
    };
    pg_query::protobuf::Node { node: Some(node) }
}

/// The expression the call `n` stands for, when it is one of these.
pub(crate) fn replacement(n: &pg_query::protobuf::Node) -> Option<pg_query::protobuf::Node> {
    let Some(N::FuncCall(f)) = n.node.as_ref() else {
        return None;
    };
    let (name, args) = plain_call(f)?;
    if let Some(template) = templates().get(&(name.as_str(), args.len())) {
        let types = template_param_types(template, args.len());
        if args.iter().zip(&types).any(|(a, t)| literal_refused(a, t)) {
            return None;
        }
        return Some(substitute(template, args));
    }
    if let Some(template) = family_template(&name, args.len()) {
        let (left, right) = family_types(&name);
        if literal_refused(&args[0], left) || literal_refused(&args[1], right) {
            return None;
        }
        let args = [typed(&args[0], left), typed(&args[1], right)];
        return Some(substitute(template, &args));
    }
    let op = operators().get(&(name.clone(), args.len()))?;
    let refused = match args {
        [l, r] => literal_refused(l, &op.left) || literal_refused(r, &op.right),
        [r] => literal_refused(r, &op.right),
        _ => false,
    };
    if refused {
        return None;
    }
    let (lexpr, rexpr) = match args {
        [l, r] => (
            Some(Box::new(typed(l, &op.left))),
            Some(Box::new(typed(r, &op.right))),
        ),
        [r] => (None, Some(Box::new(typed(r, &op.right)))),
        _ => return None,
    };
    // LIKE and ILIKE are operators with a node kind of their own.
    let kind = match op.symbol.as_str() {
        "~~" | "!~~" => pg_query::protobuf::AExprKind::AexprLike,
        "~~*" | "!~~*" => pg_query::protobuf::AExprKind::AexprIlike,
        _ => pg_query::protobuf::AExprKind::AexprOp,
    };
    let expr = pg_query::protobuf::Node {
        node: Some(N::AExpr(Box::new(pg_query::protobuf::AExpr {
            kind: kind as i32,
            name: vec![string(&op.symbol)],
            lexpr,
            rexpr,
            location: f.location,
        }))),
    };
    // A column (or any other argument whose type only planning knows) may
    // not be one the function takes (`int4pl(numeric_col, 2)` is 42883 on
    // PostgreSQL): `__alias_check` types the call's own arguments once they
    // are typed, and refuses it as PostgreSQL does.
    if args
        .iter()
        .all(|a| matches!(a.node, Some(N::AConst(_) | N::TypeCast(_))))
    {
        return Some(expr);
    }
    let mut check_args = vec![
        pg_query::protobuf::Node {
            node: Some(N::AConst(pg_query::protobuf::AConst {
                isnull: false,
                location: f.location,
                val: Some(pg_query::protobuf::a_const::Val::Sval(
                    pg_query::protobuf::String { sval: name.clone() },
                )),
            })),
        },
        expr,
    ];
    check_args.extend(args.iter().cloned());
    Some(pg_query::protobuf::Node {
        node: Some(N::FuncCall(Box::new(pg_query::protobuf::FuncCall {
            funcname: vec![string(ALIAS_CHECK)],
            args: check_args,
            funcformat: pg_query::protobuf::CoercionForm::CoerceExplicitCall as i32,
            location: f.location,
            ..Default::default()
        }))),
    })
}

/// The internal call that checks an operator-function call's argument
/// types once they are known: `(name, the operator expression, the call's
/// arguments...)`.
pub(crate) const ALIAS_CHECK: &str = "__alias_check";

#[cfg(test)]
#[path = "builtin_alias_tests.rs"]
mod tests;
