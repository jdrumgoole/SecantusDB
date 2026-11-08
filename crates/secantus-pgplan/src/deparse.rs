//! A column DEFAULT rendered the way PostgreSQL's `pg_get_expr` renders the
//! ANALYSED expression -- what `information_schema.columns.column_default`
//! and `pg_attrdef` show -- rather than the way libpg_query deparses the raw
//! parse tree.
//!
//! The two differ in exactly the ways an ORM's reflection notices: every
//! operator expression is parenthesised (`(1 + 2)`, `(1 + (2 * 3))`), an
//! untyped string literal carries the type it resolved to (`'x'::text`,
//! `'1 day'::interval`, `'d_id_seq'::regclass`) and is re-rendered through
//! that type's output function (`interval '1 hour'` is `'01:00:00'::interval`),
//! an operand the operator had to coerce shows the cast (`(2)::double
//! precision * random()`), a negative constant is quoted so it cannot re-parse
//! as a unary minus (`'-1'::integer`), SQL value functions are upper case
//! (`CURRENT_DATE`), and `LIKE` / `BETWEEN` / `IN` are rendered as the
//! operators they expand to. Measured against PostgreSQL 16.15 (2026-09-23).
//!
//! Anything this does not model falls back to libpg_query's deparse of the raw
//! node, which is still valid SQL for the default.

use pg_query::protobuf::node::Node as N;
use pg_query::protobuf::{a_const, AExpr, AExprKind, BoolExprType, SqlValueFunctionOp as Op};

use crate::{cast_value, display_type, func_name, render_value_text, scalar, type_name_of};

/// The static type of a node, as far as a default's deparse needs it:
/// `None` is an untyped string literal (PostgreSQL's `unknown`).
type Ty = Option<String>;

/// The default's text as `pg_get_expr` would print it, given the column's
/// type (which is what an untyped literal at the top level resolves to).
pub fn default_display_text(raw: &pg_query::protobuf::Node, column_type: &str) -> String {
    render(raw, ty(column_type))
}

fn ty(name: &str) -> Ty {
    Some(name.to_string())
}

fn is_int(t: &str) -> bool {
    matches!(
        t,
        "int2" | "int4" | "int8" | "smallint" | "integer" | "int" | "bigint"
    )
}

fn is_float(t: &str) -> bool {
    matches!(t, "float4" | "float8" | "real" | "double")
}

fn int_width(t: &str) -> u8 {
    match t {
        "int2" | "smallint" => 2,
        "int8" | "bigint" => 8,
        _ => 4,
    }
}

/// The type an expression has, or `None` for an untyped literal.
fn expr_type(node: &pg_query::protobuf::Node) -> Ty {
    match node.node.as_ref() {
        Some(N::AConst(c)) => match c.val.as_ref() {
            _ if c.isnull => None,
            Some(a_const::Val::Ival(_)) => ty("int4"),
            Some(a_const::Val::Fval(f)) => ty(if big_int_literal(&f.fval).is_some() {
                "int8"
            } else {
                "numeric"
            }),
            Some(a_const::Val::Boolval(_)) => ty("bool"),
            _ => None,
        },
        Some(N::TypeCast(tc)) => tc.type_name.as_ref().map(type_name_of),
        Some(N::FuncCall(f)) => {
            let name = func_name(f).unwrap_or_default();
            match name.as_str() {
                "nextval" | "currval" | "lastval" | "setval" => ty("int8"),
                "now" | "transaction_timestamp" | "statement_timestamp" | "clock_timestamp" => {
                    ty("timestamptz")
                }
                "gen_random_uuid" | "uuid_generate_v4" => ty("uuid"),
                "random" => ty("float8"),
                "extract" | "date_part" => ty("numeric"),
                "abs" | "round" | "ceil" | "floor" | "trunc" => f
                    .args
                    .first()
                    .map(|a| expr_type(a).unwrap_or_else(|| "numeric".to_string())),
                other => scalar::known_result_type(other).map(str::to_string),
            }
        }
        Some(N::SqlvalueFunction(svf)) => ty(match Op::try_from(svf.op) {
            Ok(Op::SvfopCurrentDate) => "date",
            Ok(Op::SvfopCurrentTime | Op::SvfopCurrentTimeN) => "timetz",
            Ok(Op::SvfopCurrentTimestamp | Op::SvfopCurrentTimestampN) => "timestamptz",
            Ok(Op::SvfopLocaltime | Op::SvfopLocaltimeN) => "time",
            Ok(Op::SvfopLocaltimestamp | Op::SvfopLocaltimestampN) => "timestamp",
            _ => "name",
        }),
        Some(N::AExpr(e)) => aexpr_type(e),
        Some(N::BoolExpr(_) | N::NullTest(_) | N::BooleanTest(_)) => ty("bool"),
        Some(N::CoalesceExpr(c)) => c.args.iter().find_map(expr_type),
        Some(N::MinMaxExpr(m)) => m.args.iter().find_map(expr_type),
        Some(N::CaseExpr(c)) => c
            .args
            .iter()
            .find_map(|w| match w.node.as_ref() {
                Some(N::CaseWhen(cw)) => cw.result.as_deref().and_then(expr_type),
                _ => None,
            })
            .or_else(|| c.defresult.as_deref().and_then(expr_type)),
        Some(N::AArrayExpr(a)) => a
            .elements
            .iter()
            .find_map(expr_type)
            .map(|t| format!("{t}[]"))
            .or_else(|| ty("text[]")),
        _ => None,
    }
}

/// An integer literal too wide for `int4`, which the parser hands over as a
/// float-form constant and PostgreSQL resolves to `bigint` when it fits.
fn big_int_literal(text: &str) -> Option<i64> {
    if text.chars().all(|c| c.is_ascii_digit()) {
        text.parse().ok()
    } else {
        None
    }
}

fn op_name(e: &AExpr) -> &str {
    e.name
        .last()
        .and_then(|n| n.node.as_ref())
        .and_then(|n| match n {
            N::String(s) => Some(s.sval.as_str()),
            _ => None,
        })
        .unwrap_or("")
}

/// How an operator resolves its two typed operands: `(left cast, right
/// cast, result)`, where a cast is the type that side is coerced to (`None`
/// keeps its own -- there is an operator for the pair, as for `integer +
/// bigint`, `text || integer` or `interval * double precision`).
fn resolve(op: &str, lhs: Ty, rhs: Ty) -> (Ty, Ty, Ty) {
    let comparison = matches!(
        op,
        "=" | "<>" | "!=" | "<" | ">" | "<=" | ">=" | "~~" | "!~~" | "~~*" | "!~~*" | "~" | "!~"
    );
    let result = |t: Ty| if comparison { ty("bool") } else { t };
    if op == "||" {
        return (None, None, ty("text"));
    }
    let (l, r) = match (lhs, rhs) {
        (None, None) => return (None, None, result(ty("text"))),
        (Some(l), None) | (None, Some(l)) => return (None, None, result(Some(l))),
        (Some(l), Some(r)) => (l, r),
    };
    if l == r {
        if op == "^" && is_int(&l) {
            return (ty("float8"), ty("float8"), result(ty("float8")));
        }
        return (None, None, result(Some(l)));
    }
    if is_int(&l) && is_int(&r) {
        let wide = if int_width(&l) >= int_width(&r) { l } else { r };
        return (None, None, result(Some(wide)));
    }
    let temporal = |t: &str| matches!(t, "date" | "time" | "timetz" | "timestamp" | "timestamptz");
    let numberish = |t: &str| is_int(t) || is_float(t) || t == "numeric";
    if numberish(&l) && numberish(&r) {
        let common = if is_float(&l) || is_float(&r) {
            "float8"
        } else {
            "numeric"
        };
        let cast = |t: &str| (t != common).then(|| common.to_string());
        return (cast(&l), cast(&r), result(ty(common)));
    }
    // `interval * 2`: the number becomes double precision, the interval stays.
    if l == "interval" && numberish(&r) {
        let cast = (r != "float8").then(|| "float8".to_string());
        return (None, cast, result(ty("interval")));
    }
    if r == "interval" && numberish(&l) {
        let cast = (l != "float8").then(|| "float8".to_string());
        return (cast, None, result(ty("interval")));
    }
    // date + integer, timestamp - interval, ...: each side keeps its type
    // and the result is the temporal one.
    let out = if temporal(&l) && r == "interval" {
        if l == "date" {
            ty("timestamp")
        } else {
            Some(l)
        }
    } else if temporal(&r) && l == "interval" {
        if r == "date" {
            ty("timestamp")
        } else {
            Some(r)
        }
    } else if temporal(&l) && temporal(&r) {
        if l == "date" && r == "date" {
            ty("int4")
        } else {
            ty("interval")
        }
    } else if temporal(&l) {
        Some(l)
    } else if temporal(&r) {
        Some(r)
    } else {
        ty("text")
    };
    (None, None, result(out))
}

fn aexpr_type(e: &AExpr) -> Ty {
    match AExprKind::try_from(e.kind) {
        Ok(AExprKind::AexprOp) => match (e.lexpr.as_deref(), e.rexpr.as_deref()) {
            (Some(l), Some(r)) => resolve(op_name(e), expr_type(l), expr_type(r)).2,
            (None, Some(r)) => expr_type(r),
            _ => None,
        },
        Ok(AExprKind::AexprNullif) => e.lexpr.as_deref().and_then(expr_type),
        _ => ty("bool"),
    }
}

/// Was the call written in SQL syntax (`SUBSTRING(x FROM 1)`, `TRIM(LEADING
/// FROM x)`) rather than as a function call?
fn sql_syntax(f: &pg_query::protobuf::FuncCall) -> bool {
    f.funcformat == pg_query::protobuf::CoercionForm::CoerceSqlSyntax as i32
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A string literal resolved to `target`: `'text'::type`, the text
/// re-rendered through the type's output function when it parses.
fn typed_literal(text: &str, target: &str) -> String {
    let shown = match cast_value(bson::Bson::String(text.to_string()), target) {
        Ok(v) if v != bson::Bson::Null => render_value_text(&v),
        _ => text.to_string(),
    };
    format!("{}::{}", quote(&shown), display_type(target))
}

/// A numeric literal as `pg_get_expr` prints a numeric constant: bare when
/// its output form carries a point or an exponent and no sign, else quoted
/// and cast (`1.5`, `'1000'::numeric` for `1e3`, `'-1.5'::numeric`).
fn numeric_literal(text: &str) -> String {
    let shown = match cast_value(bson::Bson::String(text.to_string()), "numeric") {
        Ok(v) => render_value_text(&v),
        Err(_) => text.to_string(),
    };
    let plain = shown
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if plain && shown.contains(['.', 'e', 'E']) && !shown.starts_with('-') {
        shown
    } else {
        format!("{}::numeric", quote(&shown))
    }
}

/// `expr` coerced to `cast` when the operator asked for one: `(expr)::type`.
fn coerced(node: &pg_query::protobuf::Node, own: &Ty, cast: &Ty) -> String {
    let text = render(node, own.clone());
    match cast {
        Some(t) => format!("({text})::{}", display_type(t)),
        None => text,
    }
}

fn render(node: &pg_query::protobuf::Node, ctx: Ty) -> String {
    let fallback = || crate::check_expression_text(node).unwrap_or_default();
    match node.node.as_ref() {
        Some(N::AConst(c)) => {
            if c.isnull {
                return format!("NULL::{}", display_type(ctx.as_deref().unwrap_or("text")));
            }
            match c.val.as_ref() {
                Some(a_const::Val::Ival(v)) => {
                    if v.ival < 0 {
                        format!("'{}'::integer", v.ival)
                    } else {
                        v.ival.to_string()
                    }
                }
                Some(a_const::Val::Fval(f)) => match big_int_literal(&f.fval) {
                    Some(n) => format!("'{n}'::bigint"),
                    None => numeric_literal(&f.fval),
                },
                Some(a_const::Val::Boolval(b)) => b.boolval.to_string(),
                Some(a_const::Val::Sval(s)) => {
                    typed_literal(&s.sval, ctx.as_deref().unwrap_or("text"))
                }
                _ => fallback(),
            }
        }
        Some(N::TypeCast(tc)) => {
            let Some(target) = tc.type_name.as_ref().map(type_name_of) else {
                return fallback();
            };
            let Some(arg) = tc.arg.as_deref() else {
                return fallback();
            };
            match arg.node.as_ref() {
                Some(N::AConst(c)) if c.isnull => {
                    format!("NULL::{}", display_type(&target))
                }
                Some(N::AConst(c)) => match c.val.as_ref() {
                    // `'5'::int` is analysed straight to an integer constant.
                    Some(a_const::Val::Sval(s)) => {
                        let (base, _) = split_typmod(&target);
                        match cast_value(bson::Bson::String(s.sval.clone()), &target) {
                            Ok(bson::Bson::Int32(n)) if base == "int4" && n >= 0 => n.to_string(),
                            Ok(bson::Bson::Int32(n)) if base == "int4" => format!("'{n}'::integer"),
                            _ => typed_literal(&s.sval, base),
                        }
                    }
                    // `5::int` is just `5`; `5::bigint` keeps its cast.
                    Some(a_const::Val::Ival(_)) if is_int(&target) && int_width(&target) == 4 => {
                        render(arg, None)
                    }
                    Some(a_const::Val::Fval(_)) if target == "numeric" => render(arg, None),
                    _ => format!("({})::{}", render(arg, None), display_type(&target)),
                },
                _ => format!("({})::{}", render(arg, None), display_type(&target)),
            }
        }
        Some(N::SqlvalueFunction(svf)) => match Op::try_from(svf.op) {
            Ok(Op::SvfopCurrentDate) => "CURRENT_DATE".into(),
            Ok(Op::SvfopCurrentTime | Op::SvfopCurrentTimeN) => "CURRENT_TIME".into(),
            Ok(Op::SvfopCurrentTimestamp | Op::SvfopCurrentTimestampN) => {
                "CURRENT_TIMESTAMP".into()
            }
            Ok(Op::SvfopLocaltime | Op::SvfopLocaltimeN) => "LOCALTIME".into(),
            Ok(Op::SvfopLocaltimestamp | Op::SvfopLocaltimestampN) => "LOCALTIMESTAMP".into(),
            Ok(Op::SvfopCurrentRole) => "CURRENT_ROLE".into(),
            Ok(Op::SvfopCurrentUser) => "CURRENT_USER".into(),
            Ok(Op::SvfopUser) => "USER".into(),
            Ok(Op::SvfopSessionUser) => "SESSION_USER".into(),
            Ok(Op::SvfopCurrentCatalog) => "CURRENT_CATALOG".into(),
            Ok(Op::SvfopCurrentSchema) => "CURRENT_SCHEMA".into(),
            _ => fallback(),
        },
        Some(N::FuncCall(f)) => {
            let name = func_name(f).unwrap_or_default();
            let arg_ty = |i: usize| -> Ty {
                match (name.as_str(), i) {
                    ("nextval" | "currval" | "setval", 0) => ty("regclass"),
                    _ => ty("text"),
                }
            };
            let args: Vec<String> = f
                .args
                .iter()
                .enumerate()
                .map(|(i, a)| render(a, expr_type(a).or_else(|| arg_ty(i))))
                .collect();
            match (name.as_str(), args.as_slice()) {
                ("btrim", [s]) => format!("TRIM(BOTH FROM {s})"),
                ("ltrim", [s]) if sql_syntax(f) => format!("TRIM(LEADING FROM {s})"),
                ("rtrim", [s]) if sql_syntax(f) => format!("TRIM(TRAILING FROM {s})"),
                ("substring", [s, from]) if sql_syntax(f) => {
                    format!("SUBSTRING({s} FROM {from})")
                }
                ("substring", [s, from, len]) if sql_syntax(f) => {
                    format!("SUBSTRING({s} FROM {from} FOR {len})")
                }
                ("extract", [field, from]) => {
                    let field = field
                        .trim_start_matches('\'')
                        .split('\'')
                        .next()
                        .unwrap_or("");
                    format!("EXTRACT({field} FROM {from})")
                }
                _ => format!("{name}({})", args.join(", ")),
            }
        }
        Some(N::AExpr(e)) => render_aexpr(e, node, &fallback),
        Some(N::BoolExpr(b)) => {
            let parts: Vec<String> = b.args.iter().map(|a| render(a, ty("bool"))).collect();
            match BoolExprType::try_from(b.boolop) {
                Ok(BoolExprType::AndExpr) => format!("({})", parts.join(" AND ")),
                Ok(BoolExprType::OrExpr) => format!("({})", parts.join(" OR ")),
                Ok(BoolExprType::NotExpr) => format!("(NOT {})", parts.join("")),
                _ => fallback(),
            }
        }
        Some(N::NullTest(t)) => {
            let Some(arg) = t.arg.as_deref() else {
                return fallback();
            };
            let is_not = t.nulltesttype == pg_query::protobuf::NullTestType::IsNotNull as i32;
            format!(
                "({} IS {}NULL)",
                render(arg, expr_type(arg)),
                if is_not { "NOT " } else { "" }
            )
        }
        Some(N::BooleanTest(t)) => {
            use pg_query::protobuf::BoolTestType as B;
            let Some(arg) = t.arg.as_deref() else {
                return fallback();
            };
            let what = match B::try_from(t.booltesttype) {
                Ok(B::IsTrue) => "IS TRUE",
                Ok(B::IsNotTrue) => "IS NOT TRUE",
                Ok(B::IsFalse) => "IS FALSE",
                Ok(B::IsNotFalse) => "IS NOT FALSE",
                Ok(B::IsUnknown) => "IS UNKNOWN",
                Ok(B::IsNotUnknown) => "IS NOT UNKNOWN",
                _ => return fallback(),
            };
            format!("({} {what})", render(arg, ty("bool")))
        }
        Some(N::CoalesceExpr(c)) => {
            let common = c.args.iter().find_map(expr_type).or(ctx);
            let parts: Vec<String> = c.args.iter().map(|a| render(a, common.clone())).collect();
            format!("COALESCE({})", parts.join(", "))
        }
        Some(N::MinMaxExpr(m)) => {
            let common = m.args.iter().find_map(expr_type).or(ctx);
            let parts: Vec<String> = m.args.iter().map(|a| render(a, common.clone())).collect();
            let name = if m.op == pg_query::protobuf::MinMaxOp::IsGreatest as i32 {
                "GREATEST"
            } else {
                "LEAST"
            };
            format!("{name}({})", parts.join(", "))
        }
        Some(N::CaseExpr(c)) => {
            let result_ty = expr_type(node).or(ctx);
            let test_ty = c.arg.as_deref().and_then(expr_type);
            let mut out = String::from("\nCASE");
            if let Some(arg) = c.arg.as_deref() {
                out.push(' ');
                out.push_str(&render(arg, test_ty.clone()));
            }
            for w in &c.args {
                let Some(N::CaseWhen(cw)) = w.node.as_ref() else {
                    continue;
                };
                let (Some(cond), Some(res)) = (cw.expr.as_deref(), cw.result.as_deref()) else {
                    continue;
                };
                let cond_ty = if c.arg.is_some() {
                    test_ty.clone()
                } else {
                    ty("bool")
                };
                out.push_str(&format!(
                    "\n    WHEN {} THEN {}",
                    render(cond, cond_ty),
                    render(res, result_ty.clone())
                ));
            }
            if let Some(d) = c.defresult.as_deref() {
                out.push_str(&format!("\n    ELSE {}", render(d, result_ty)));
            }
            out.push_str("\nEND");
            out
        }
        Some(N::AArrayExpr(a)) => {
            let elem = a
                .elements
                .iter()
                .find_map(expr_type)
                .or_else(|| {
                    ctx.as_deref()
                        .and_then(|t| t.strip_suffix("[]"))
                        .map(str::to_string)
                })
                .or_else(|| ty("text"));
            let parts: Vec<String> = a.elements.iter().map(|e| render(e, elem.clone())).collect();
            format!("ARRAY[{}]", parts.join(", "))
        }
        _ => fallback(),
    }
}

fn split_typmod(t: &str) -> (&str, &str) {
    match t.find('(') {
        Some(i) => (&t[..i], &t[i..]),
        None => (t, ""),
    }
}

fn render_aexpr(
    e: &AExpr,
    node: &pg_query::protobuf::Node,
    fallback: &dyn Fn() -> String,
) -> String {
    let op = op_name(e);
    match AExprKind::try_from(e.kind) {
        Ok(AExprKind::AexprOp) => match (e.lexpr.as_deref(), e.rexpr.as_deref()) {
            (Some(l), Some(r)) => binary(op, l, r),
            (None, Some(r)) => {
                let rt = expr_type(r);
                format!("({op} {})", render(r, rt))
            }
            _ => fallback(),
        },
        Ok(AExprKind::AexprLike | AExprKind::AexprIlike) => {
            let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
                return fallback();
            };
            let like = AExprKind::try_from(e.kind) == Ok(AExprKind::AexprLike);
            let sym = match (like, op.starts_with('!')) {
                (true, false) => "~~",
                (true, true) => "!~~",
                (false, false) => "~~*",
                (false, true) => "!~~*",
            };
            binary(sym, l, r)
        }
        Ok(AExprKind::AexprBetween | AExprKind::AexprNotBetween) => {
            let (Some(l), Some(N::List(bounds))) = (
                e.lexpr.as_deref(),
                e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
            ) else {
                return fallback();
            };
            let [lo, hi] = bounds.items.as_slice() else {
                return fallback();
            };
            if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprBetween) {
                format!("({} AND {})", binary(">=", l, lo), binary("<=", l, hi))
            } else {
                format!("({} OR {})", binary("<", l, lo), binary(">", l, hi))
            }
        }
        Ok(AExprKind::AexprIn) => {
            let (Some(l), Some(N::List(items))) = (
                e.lexpr.as_deref(),
                e.rexpr.as_deref().and_then(|r| r.node.as_ref()),
            ) else {
                return fallback();
            };
            let lt = expr_type(l);
            let elem = lt
                .clone()
                .or_else(|| items.items.iter().find_map(expr_type))
                .or_else(|| ty("text"));
            let parts: Vec<String> = items
                .items
                .iter()
                .map(|i| render(i, elem.clone()))
                .collect();
            let (sym, quant) = if op == "<>" {
                ("<>", "ALL")
            } else {
                ("=", "ANY")
            };
            format!(
                "({} {sym} {quant} (ARRAY[{}]))",
                render(l, lt.or(elem)),
                parts.join(", ")
            )
        }
        Ok(AExprKind::AexprNullif) => {
            let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
                return fallback();
            };
            let common = expr_type(l).or_else(|| expr_type(r)).or_else(|| ty("text"));
            format!(
                "NULLIF({}, {})",
                render(l, common.clone()),
                render(r, common)
            )
        }
        Ok(AExprKind::AexprDistinct | AExprKind::AexprNotDistinct) => {
            let (Some(l), Some(r)) = (e.lexpr.as_deref(), e.rexpr.as_deref()) else {
                return fallback();
            };
            let common = expr_type(l).or_else(|| expr_type(r)).or_else(|| ty("text"));
            let not = if AExprKind::try_from(e.kind) == Ok(AExprKind::AexprNotDistinct) {
                "NOT "
            } else {
                ""
            };
            format!(
                "({} IS {not}DISTINCT FROM {})",
                render(l, common.clone()),
                render(r, common)
            )
        }
        _ => {
            let _ = node;
            fallback()
        }
    }
}

/// `(lhs op rhs)` with each operand typed, and coerced, as the operator
/// resolves them. An untyped literal takes the other side's type (`'a' ||
/// 1` still types the literal as text, since `||` is the text operator).
fn binary(op: &str, l: &pg_query::protobuf::Node, r: &pg_query::protobuf::Node) -> String {
    let (lt, rt) = (expr_type(l), expr_type(r));
    let (lc, rc, _) = resolve(op, lt.clone(), rt.clone());
    let side = |n: &pg_query::protobuf::Node, own: &Ty, cast: &Ty, other: &Ty| -> String {
        match own {
            None => {
                let literal_ty = if op == "||" {
                    ty("text")
                } else {
                    other.clone().or_else(|| ty("text"))
                };
                render(n, literal_ty)
            }
            Some(_) => coerced(n, own, cast),
        }
    };
    format!(
        "({} {op} {})",
        side(l, &lt, &lc, &rt),
        side(r, &rt, &rc, &lt)
    )
}
